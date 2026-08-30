//! Transports (decision 0438) — the client-computed cycle, two drives, one subsystem.
//!
//! The server never streams a transport's position. It sends one **anchor** — the movement
//! block's `UPDATE_FLAG_TRANSPORT` u32, the server's path-progress ms clock at create — and the
//! client computes everything else, per GameObject type:
//!
//! - **Type 15 (`MO_TRANSPORT`, boats/zeppelins):** the whole timetable (per-leg Catmull-Rom
//!   splines + stop-to-stop trapezoid times, [`benilla_formats::TransportTimetable`]) from
//!   `TaxiPathNode.dbc` and the template's `(taxiPathId, moveSpeed, accelRate)` tuple; the boat
//!   sits at `timetable((anchor + local_elapsed) % period)` every frame.
//! - **Type 11 (`TRANSPORT`, elevators/lifts/trams — decision 0438 phase 3's second consumer):**
//!   the authored keyframe path from `TransportAnimation.dbc` keyed by the **template entry**;
//!   the car sits at `spawn + R(spawn_quat)·lerp(keyframes, (anchor + local_elapsed) % period)`
//!   ([`benilla_formats::elevator_sample`] — the client's `gameobject_path_eval`, byte-verified
//!   in wow-re; vmangos `ElevatorTransport::Update` is the same math server-side).
//!
//! Re-creates (map transitions, the server's mid-course "update frames") re-anchor the clock;
//! decision 0318's replace semantics make that free.
//!
//! The armed entity is the ordinary streamed GameObject — the display (WMO for boats, M2 for
//! elevator cars), its collider, and its `Transform` all pre-exist on the GO-entity path
//! (decision 0438 §2); this module only *moves* it. Riding hangs off the same component set for
//! both drives — the mover's platform frame keys on [`Transport`], not on the drive.

use std::collections::HashMap;
use std::sync::Arc;
use bevy::platform::time::Instant;

// avian's own public AABB/tree republish system, registered by name below. Aliased because the
// chain's concern is *when* it runs, not whose it is — and because the upstream name says what it
// does to colliders, not why this app needs it a second time each frame.
use avian3d::collider_tree::update_moved_collider_aabbs as republish_moved_collider_aabbs;
use avian3d::prelude::{Collider, Position, RigidBody, Rotation};
use benilla_formats::{
    elevator_period_ms, elevator_sample, load_elevator_paths, load_taxi_path_nodes,
    ElevatorKeyframe, ElevatorPaths, TaxiPathNodes, TransportSample, TransportTimetable,
};
use bevy::prelude::*;

use crate::go_templates::GameObjectTemplates;
use crate::net::Guid;
use benilla_assets::{LockRecover, WorldAssets};
use benilla_world::ride_frame::RideFrame;
use benilla_world::vis_chain::VisChainOnly;
use benilla_world::world_map::CurrentMap;

pub(crate) struct TransportPlugin;

impl Plugin for TransportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Timetables>()
            // `.after(AssetSet::Open)` is load-bearing: the chain resource lands via Commands in
            // that set, and an unordered Startup system races it and silently finds no
            // `WorldAssets` (bit us on the first live run — every downstream log went quiet).
            .add_systems(
                Startup,
                setup_taxi_nodes.after(benilla_assets::AssetSet::Open),
            )
            .add_systems(
                Update,
                // After the net drain (fresh anchors/re-anchors applied), before the player's
                // input stage — the boat must be at its this-frame pose before the mover reads
                // the world (phase 2's platform carry rides exactly this edge).
                //
                // **[`republish_moved_collider_aabbs`] is the third link, and it is here because
                // of a general engine truth, not a lift one** (decision 1663): *a collider moved
                // in `Update` is invisible to `Update`'s own spatial queries until the next
                // physics step, by up to one frame of its own travel.* avian refreshes
                // `ColliderAabb`, `EnlargedAabb` and the `ColliderTrees` proxies in
                // `ColliderTreeSystems::UpdateAabbs`, which sits in `PhysicsSchedule` — i.e. in
                // `FixedPostUpdate`, **before** `Update` — while [`tick_transports`] writes the
                // deck's pose *in* `Update`. Every `Update`-side spatial query
                // (`SpatialQuery::aabb_intersections_with_aabb`, which is how the mover's
                // ground/step/slide casts enumerate candidates — `benilla_world::collision`) is
                // therefore pruned against the deck's **previous-frame** box.
                //
                // At 60 Hz that is centimetres and harmless. On one long frame — a backgrounded
                // window is at least one ~1 s frame (decisions 0713/0906) — a Thunder Bluff lift
                // descends 7.3 yd while its box stays put, the box ends up *above* the rider it
                // is carrying, the down-probe enumerates no deck at all, and the rider is
                // declared airborne on the very platform they are standing on. Two such frames in
                // a row and `Time<Virtual>`'s clamped 250 ms lands a full gravity step on the
                // second one: the body drops 1.2 yd through the deck and never re-earns it,
                // because the deck is now above the probe. That is the reported fall-through, and
                // 1663 records the measured chain.
                //
                // The system is avian's own, public, and read-only on `LastPhysicsTick` — it
                // writes no tick resource, so running it a second time in the frame is
                // idempotent, and the next physics step simply recomputes what has not moved
                // since. It sweeps *every* collider, not just transports; 1663 records the
                // measured per-frame cost of that sweep in a streamed outdoor scene.
                (
                    arm_transports,
                    tick_transports,
                    republish_moved_collider_aabbs::<Collider>,
                    compose_riders,
                )
                    .chain()
                    .after(benilla_world::schedule::WorldStage::Net)
                    .before(benilla_world::schedule::WorldStage::Input),
            )
            // …and the ride frame LAST, after the mover has decided what we are standing on this
            // frame ([`crate::player::Player::ride`] is written inside `PlayerControlSet`). Its
            // consumers — the particle and ribbon sims — run in `PostUpdate`, so the deferred
            // insert has landed by the time anything reads it.
            .add_systems(
                Update,
                stamp_ride_frames.after(crate::player::PlayerControlSet),
            );
    }
}

/// The create-block anchor: the server's path-progress ms + the local instant it arrived.
/// Inserted (and re-inserted, on every re-create) by the net apply's `object_create` arm for a
/// GameObject whose movement block carried `UPDATE_FLAG_TRANSPORT`.
#[derive(Component)]
pub(crate) struct TransportAnchor {
    pub(crate) progress_ms: u32,
    pub(crate) at: Instant,
}

/// A pending type-11 elevator/lift: everything its arm needs, captured off the create block by
/// the net apply (no template round-trip — the keyframe path is keyed by the template **entry**,
/// which the create's `OBJECT_FIELD_ENTRY` already carries). Replaced by [`Transport`] once the
/// keyframe catalog answers; a GO whose entry has no authored path stays seeded-and-frozen (the
/// same silence the reference affords it).
#[derive(Component)]
pub(crate) struct ElevatorSeed {
    /// `gameobject_template` entry — `TransportAnimation.dbc`'s `TransportID` key.
    pub(crate) entry: u32,
    /// The stationary spawn position (raw WoW coords) — the movement block's `HAS_POSITION`
    /// slot (a transport create never carries `GAMEOBJECT_POS_*`).
    pub(crate) base_pos: [f32; 3],
    /// The spawn facing (wire orientation, WoW radians) — the car's constant rendered heading.
    pub(crate) yaw: f32,
    /// `GAMEOBJECT_ROTATION` — the spawn's local-rotation quaternion the keyframe offsets
    /// rotate through. `None` on a wire that carried no component (falls back to a pure-yaw
    /// quat built from `yaw`, which is exactly what the live spawn rows encode anyway).
    pub(crate) quat: Option<[f32; 4]>,
}

/// A ticking transport. Armed by [`arm_transports`]: a boat once the ask-once template answer
/// (the `(taxiPathId, moveSpeed, accelRate)` tuple) lands; an elevator as soon as the keyframe
/// catalog is open (its create block already carried everything else).
#[derive(Component)]
pub(crate) struct Transport {
    drive: Drive,
    /// Last frame's `moving` — the dock/depart edge detector behind the trace log (the phase-1
    /// reconciliation instrument: a "the boat isn't where the server thinks" report is settled
    /// against these timestamps, never by eye — decision 0438 phasing).
    was_moving: bool,
}

/// The two cycle evaluators behind one component (decision 0438 §5: elevators are "the same
/// subsystem, second consumer" — the rider frame, the worldport spare, and the instruments all
/// key on [`Transport`] and never care which drive is underneath).
enum Drive {
    /// Type 15: the client-computed taxi-path timetable.
    Taxi(Arc<TransportTimetable>),
    /// Type 11: the authored `TransportAnimation` keyframe path.
    Lift(Lift),
}

/// **No map field, deliberately** (decision 1654). A lift's map is not a datum it owns: vmangos
/// hands a player the whole map's transport set on entry (`Map::SendInitTransports`) and takes it
/// back on the way out, so a type-11 is only ever resident for someone standing on its own map —
/// the car is always on the map *you* are on. 0611 stamped `CurrentMap` here at arm time, which in
/// the login frame is the startup seed (`arm_transports` runs a stage before `player::wire_in`
/// writes the real map), and the off-map hide below then hid every lift on Kalimdor for the whole
/// session — the director's Thunder Bluff report, and B168's Gnomeregan lift.
struct Lift {
    frames: Arc<Vec<ElevatorKeyframe>>,
    period_ms: u32,
    base_pos: [f32; 3],
    quat: [f32; 4],
    /// Constant rendered heading (wire orientation, WoW radians).
    yaw: f32,
}

impl Transport {
    /// Which drive is underneath — for the instruments only (`WOW_LIFT_CENSUS`); no consumer
    /// keys on it (decision 0438 §5: the drive is private by design).
    pub(crate) fn drive_label(&self) -> &'static str {
        match &self.drive {
            Drive::Taxi(_) => "taxi",
            Drive::Lift(_) => "lift",
        }
    }

    pub(crate) fn period_ms(&self) -> u32 {
        match &self.drive {
            Drive::Taxi(t) => t.period_ms,
            Drive::Lift(l) => l.period_ms,
        }
    }

    /// The transport's live cycle position for an anchor — the rider math needs the same
    /// number the tick used, so it lives here.
    pub(crate) fn cycle_ms(&self, anchor: &TransportAnchor) -> u32 {
        let progress = u64::from(anchor.progress_ms) + anchor.at.elapsed().as_millis() as u64;
        (progress % u64::from(self.period_ms().max(1))) as u32
    }

    /// Whether any leg of this transport's cycle lies on `map_id` — the cross-map worldport's
    /// spare predicate (decision 0455). **A lift always answers no** (1654): the spare exists so a
    /// *ridden boat* survives the seam mid-cycle, and a lift never spans one — the server re-sends
    /// the destination map's whole transport set on arrival (`SendInitTransports`), so a lift
    /// despawns and streams back like every other object.
    pub(crate) fn touches_map(&self, map_id: u32) -> bool {
        match &self.drive {
            Drive::Taxi(t) => t.touches_map(map_id),
            Drive::Lift(_) => false,
        }
    }

    /// The cycle sample — position/heading/map/motion at `cycle_ms`, both drives. `here` is the
    /// map the viewer is on: a **taxi** ignores it (its timetable knows which continent each leg
    /// lies on, which is the whole point of the off-map hide), and a **lift** reports it, because
    /// a type-11 exists only for players standing on its own map. That is the honest answer to
    /// "which map is this position expressed in", and it is what [`Lift`] refuses to store.
    fn sample(&self, cycle_ms: u32, here: u32) -> TransportSample {
        match &self.drive {
            Drive::Taxi(t) => t.sample(cycle_ms),
            Drive::Lift(l) => {
                let (pos, moving) = elevator_sample(&l.frames, l.base_pos, l.quat, cycle_ms);
                TransportSample {
                    map: here,
                    pos,
                    heading: l.yaw,
                    moving,
                }
            }
        }
    }

    /// The live sample for an anchor — the same pose the tick writes this frame. For
    /// instruments (the crossing probe reads dock state + the deck drop point off it).
    pub(crate) fn sample_at(&self, anchor: &TransportAnchor, here: u32) -> TransportSample {
        self.sample(self.cycle_ms(anchor), here)
    }
}

/// The transport path data: `TaxiPathNode.dbc` + the per-path timetables built from it, shared
/// across instances (the Menethil boat and its return-leg twin share one build; `None` in
/// `built` = a failed build, remembered so it isn't retried every frame) — and the
/// `TransportAnimation.dbc` keyframe catalog for the type-11 lifts, shared the same way (the
/// two Mesa cars of one entry share one `Arc`).
#[derive(Resource, Default)]
struct Timetables {
    nodes: Option<TaxiPathNodes>,
    built: HashMap<u32, Option<Arc<TransportTimetable>>>,
    elevators: Option<ElevatorPaths>,
    lifts: HashMap<u32, Arc<Vec<ElevatorKeyframe>>>,
}

fn setup_taxi_nodes(mut timetables: ResMut<Timetables>, world_assets: Option<Res<WorldAssets>>) {
    let Some(world_assets) = world_assets else {
        return;
    };
    let mut chain = world_assets.chain.lock_recover();
    match load_taxi_path_nodes(&mut chain) {
        Ok(nodes) => {
            info!("taxi path nodes: {} paths", nodes.len());
            timetables.nodes = Some(nodes);
        }
        // No taxi data ⇒ boats render frozen at their create pose — degraded, not broken.
        Err(e) => warn!("taxi path nodes unavailable, transports stay put: {e:#}"),
    }
    match load_elevator_paths(&mut chain) {
        Ok(paths) => {
            info!("transport animations: {} lift paths", paths.len());
            timetables.elevators = Some(paths);
        }
        // Same degradation as the boats: no keyframes ⇒ cars park at their spawn point.
        Err(e) => warn!("TransportAnimation unavailable, lifts stay put: {e:#}"),
    }
}

/// Arm anchored GameObjects: a **boat** (type 15) once its template answer (the taxi tuple)
/// lands — build (or reuse) the path's timetable; a **lift** (type 11, [`ElevatorSeed`]) as
/// soon as the keyframe catalog is open — its create block already carried everything else.
/// The query is a handful of entities at most (the map's transports), so polling is free; the
/// boat's ask-once template request was already made at create (`go_templates.request` in the
/// apply arm).
///
/// **The lift arm reads no world state at all** (1654): at login every type-11 on the map arms in
/// the same drain that carries `SMSG_LOGIN_VERIFY_WORLD`, one stage before `player::wire_in`
/// writes `CurrentMap` — so anything captured from the world *here* is captured stale.
#[allow(clippy::type_complexity)] // one Bevy system's full input set
fn arm_transports(
    mut commands: Commands,
    mut timetables: ResMut<Timetables>,
    templates: Res<GameObjectTemplates>,
    // `Or<…, With<ElevatorSeed>>`: a re-created lift is already armed (`Transport` present) but
    // carries a fresh seed from the new create block — it re-enters here so the seed is consumed
    // (a fresh, identical arm) instead of lingering. Boats never carry a seed, so for them the
    // filter is exactly `Without<Transport>` as before.
    pending: Query<
        (Entity, &Guid, Option<&ElevatorSeed>),
        (
            With<TransportAnchor>,
            Or<(Without<Transport>, With<ElevatorSeed>)>,
        ),
    >,
) {
    if pending.is_empty() {
        return;
    }
    for (entity, guid, seed) in &pending {
        // — the lift arm (decision 0438 phase 3's second consumer) —
        if let Some(seed) = seed {
            let Timetables {
                elevators, lifts, ..
            } = &mut *timetables;
            let Some(paths) = elevators.as_ref() else {
                continue; // no client data — stays frozen
            };
            let Some(frames) = paths.entry(seed.entry) else {
                // No authored path for this entry: park it at its spawn pose for good (the
                // real client's behaviour for a pathless type-11). The anchor goes too, so
                // the entity leaves this query instead of re-polling as a mistaken boat — and
                // the spawn-hidden state lifts (anchored creates spawn `Hidden` until their
                // first ticked pose, which a parked car never gets).
                warn!(
                    "transport: no TransportAnimation path for entry {} (guid {:#x}) — parked",
                    seed.entry, guid.0
                );
                commands
                    .entity(entity)
                    .remove::<(ElevatorSeed, TransportAnchor)>()
                    .insert(Visibility::default())
                    // The one insert-based visibility flip: `Visibility`'s require chain just
                    // re-added the sweep row a net root sheds at spawn — strip it again
                    // (benilla_world::vis_chain).
                    .vis_chain_only();
                continue;
            };
            let frames = lifts
                .entry(seed.entry)
                .or_insert_with(|| Arc::new(frames.to_vec()))
                .clone();
            let period_ms = elevator_period_ms(&frames);
            // A wire that carried no rotation quat falls back to the pure-yaw quat of the
            // spawn facing — the identical encoding every live 1.12 spawn row uses.
            let quat = seed.quat.unwrap_or_else(|| {
                let (s, c) = (seed.yaw * 0.5).sin_cos();
                [0.0, 0.0, s, c]
            });
            debug!(
                "transport: lift entry {} armed — period {period_ms} ms (guid {:#x})",
                seed.entry, guid.0
            );
            commands.entity(entity).remove::<ElevatorSeed>().insert((
                Transport {
                    drive: Drive::Lift(Lift {
                        frames,
                        period_ms,
                        base_pos: seed.base_pos,
                        quat,
                        yaw: seed.yaw,
                    }),
                    was_moving: false,
                },
                // Kinematic for the same reason as the boats below.
                RigidBody::Kinematic,
            ));
            continue;
        }

        // — the boat arm —
        let Some(mo) = templates.get(guid.0).and_then(|t| t.mo_transport) else {
            continue; // template answer not in yet — next frame
        };
        let Timetables { nodes, built, .. } = &mut *timetables;
        let Some(nodes) = nodes.as_ref() else {
            continue; // no client data — stays frozen
        };
        let timetable = built
            .entry(mo.taxi_path_id)
            .or_insert_with(|| {
                let t = nodes.path(mo.taxi_path_id).and_then(|path| {
                    // The build self-pins its cycle length to the real client's own bookkeeping
                    // (bit-exact against all nine server-sniff periods — the 2026-07-17 wow-re
                    // §5 verdict), so the period on the label IS the server's.
                    let t = TransportTimetable::build(path, mo.move_speed, mo.accel_rate)?;
                    info!(
                        "transport: path {} timetable built — period {} ms",
                        mo.taxi_path_id, t.period_ms
                    );
                    Some(t)
                });
                if t.is_none() {
                    warn!(
                        "transport: path {} timetable build FAILED (guid {:#x})",
                        mo.taxi_path_id, guid.0
                    );
                }
                t.map(Arc::new)
            })
            .clone();
        if let Some(timetable) = timetable {
            // Kinematic, not the GO-attach default of Static: this body moves every frame, and
            // avian rebuilds the kinematic collider tree per step by design (the static tree is
            // incremental, tuned for immobile geometry). The mover's casts see it either way —
            // spatial queries traverse all trees — but the label matches the motion.
            commands.entity(entity).insert((
                Transport {
                    drive: Drive::Taxi(timetable),
                    was_moving: false,
                },
                RigidBody::Kinematic,
            ));
        }
    }
}

/// The per-frame transport tick — the client's own `(progress + anchor) % period` leg walk
/// (wow-re `0x5f50a0`, decision 0438): sample the timetable, write the pose. Translation +
/// rotation only — the renderer bakes model scale into the transform (`write_pose`'s law).
/// Off-map samples (the boat is sailing the other continent's leg) hide the model; the server
/// removes the GO around the same time, so this is belt-and-braces for the transition frames.
/// **A lift can never trip it** (decision 1654): its sample is expressed in the map passed in, so
/// the term is a boat's alone — which is what it always meant, and what 0611's stamped map broke.
#[allow(clippy::type_complexity)] // one Bevy system's full input set
fn tick_transports(
    current_map: Option<Res<CurrentMap>>,
    mut transports: Query<(
        &Guid,
        &mut Transport,
        &TransportAnchor,
        &mut Transform,
        &mut Visibility,
        Option<&mut Position>,
        Option<&mut Rotation>,
    )>,
) {
    let Some(current_map) = current_map else {
        return;
    };
    for (guid, mut transport, anchor, mut transform, mut visibility, position, rotation) in
        &mut transports
    {
        let cycle = transport.cycle_ms(anchor);
        let sample = transport.sample(cycle, current_map.0);
        if sample.moving != transport.was_moving {
            transport.was_moving = sample.moving;
            debug!(
                "transport {:#x}: {} at cycle {cycle} ms (period {}, map {})",
                guid.0,
                if sample.moving { "departed" } else { "docked" },
                transport.period_ms(),
                sample.map,
            );
        }
        // Write-on-change only, on every component here (1473): a docked leg samples the
        // identical pose each frame, and the unconditional writes this replaces re-dirtied the
        // deck's whole subtree — transform propagation over every submesh plus a `Visibility`
        // re-propagation — for every docked elevator on the map, every frame.
        // Boats only, by construction — see this system's doc.
        if sample.map != current_map.0 {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        }
        visibility.set_if_neq(Visibility::Inherited);
        let translation = benilla_assets::coords::wow_to_bevy(sample.pos);
        let heading = Quat::from_rotation_y(sample.heading);
        if transform.translation != translation || transform.rotation != heading {
            transform.translation = translation;
            transform.rotation = heading;
        }
        // Mirror the pose into avian's own components: its Transform→Position sync runs in the
        // physics schedule (before Update), so without this the mover's same-frame casts would
        // see the deck one frame behind — ~0.5 yd astern at cruise. The broad-phase AABB still
        // lags a step, which is harmless (a rider stands deep inside the boat's AABB).
        if let Some(mut p) = position {
            if p.0 != transform.translation {
                p.0 = transform.translation;
            }
        }
        if let Some(mut r) = rotation {
            if r.0 != transform.rotation {
                r.0 = transform.rotation;
            }
        }
    }
}

/// An observed mover's pose in a transport's local frame — the `MOVEFLAG_ONTRANSPORT` tail of
/// its `LIVING` create block or `MSG_MOVE_*` relay (decision 0438 phase 2: "observed riders stop
/// discarding it"). [`compose_riders`] re-anchors the entity through the named transport's live
/// matrix every frame; the local pose itself only changes when the next packet lands (no
/// between-packet extrapolation in the deck frame yet — deck NPCs stand still, and a walking
/// rider corrects at its packet rate). Inserted/updated/removed by the net apply.
#[derive(Component)]
pub(crate) struct TransportRider {
    pub(crate) transport_guid: u64,
    /// Rider position in the transport's local frame, raw WoW coords (as on the wire).
    pub(crate) local_pos: [f32; 3],
    /// Rider facing in the transport's local frame (WoW radians) — world facing is
    /// `local + transport yaw` (the GetAbsoluteFacing law).
    pub(crate) local_orientation: f32,
}

/// Compose every observed rider's world pose through its transport's this-frame matrix — chained
/// after [`tick_transports`], so the deck carries its riders in the same frame it moves. Also
/// refreshes [`RemoteMotion`]'s canonical WoW pose so every downstream consumer (nameplates, the
/// animation selector, the landing predictor) sees the carried position; the extrapolator ran
/// earlier (WorldStage::Net) and this simply wins the frame. A rider whose boat isn't streamed
/// in (or is off-map) keeps its last relayed world pose — the server despawns the pair together.
fn compose_riders(
    guid_index: Res<crate::net::GuidIndex>,
    boats: Query<&Transform, (With<Transport>, Without<TransportRider>)>,
    mut riders: Query<
        (
            &TransportRider,
            &mut Transform,
            Option<&mut crate::net::RemoteMotion>,
        ),
        Without<Transport>,
    >,
) {
    for (rider, mut transform, motion) in &mut riders {
        let Some(&boat_entity) = guid_index.0.get(&rider.transport_guid) else {
            continue;
        };
        let Ok(boat) = boats.get(boat_entity) else {
            continue;
        };
        let local = benilla_assets::coords::wow_to_bevy(rider.local_pos);
        let world = boat.translation + boat.rotation * local;
        let yaw = rider.local_orientation + boat.rotation.to_euler(EulerRot::YXZ).0;
        transform.translation = world;
        transform.rotation = Quat::from_rotation_y(yaw);
        if let Some(mut rm) = motion {
            rm.wow_pos = benilla_assets::coords::bevy_to_wow(world);
            rm.orientation = yaw;
        }
    }
}

/// Publish the **ride frame** onto every model standing on a transport — the reference's
/// `SetMoveBase` install (`0x617170`/`0x618970` → `0x718910` → `[CM2Model+0x17c]`), which is the
/// frame a rider's world-space effects are *stored* in ([`benilla_world::ride_frame`]).
///
/// Two riders, one component. The **body we steer** takes the mover's own platform attach
/// ([`crate::player::Player::ride_entity`] — the deck collider that grounded us), and every
/// **observed** rider takes the wire's transport tail ([`TransportRider`]). Everything hung off
/// either — a held weapon, its enchant streamer, a spell kit — inherits it through the
/// `ParentModel` chain rather than being stamped itself, which is exactly how the reference
/// propagates `[model+0x17c]` to a model's children every frame (`0x7142c1` in `m2_animate`).
///
/// Why this is a component and not a field on `Transport`: the fact is *per rider*, it changes on
/// a step, and `benilla-world` — which owns the emitters that consume it — cannot see either
/// `Player` or `TransportRider`. Decision 1591 named this seam and left it unbuilt; the director's
/// weapon-trail report on the Thunder Bluff lift is what it costs.
fn stamp_ride_frames(
    mut commands: Commands,
    player: Res<crate::player::Player>,
    guid_index: Res<crate::net::GuidIndex>,
    body: Query<Entity, With<crate::net::Embodied>>,
    riders: Query<(Entity, &TransportRider)>,
    stamped: Query<(Entity, &RideFrame)>,
) {
    let mut want: HashMap<Entity, Entity> = HashMap::new();
    if let (Ok(me), Some(deck)) = (body.single(), player.ride_entity()) {
        want.insert(me, deck);
    }
    for (rider, on) in &riders {
        // A rider whose transport isn't streamed in keeps no frame: there is no pose to store
        // against, and the server despawns the pair together anyway (`compose_riders`' note).
        if let Some(&deck) = guid_index.0.get(&on.transport_guid) {
            want.insert(rider, deck);
        }
    }
    // Reconcile rather than re-stamp: an unchanged rider must not touch its component at all, so
    // change detection stays quiet on a deck full of passengers.
    for (model, current) in &stamped {
        match want.remove(&model) {
            Some(deck) if deck == current.0 => {}
            Some(deck) => {
                commands.entity(model).insert(RideFrame(deck));
            }
            None => {
                commands.entity(model).remove::<RideFrame>();
            }
        }
    }
    for (model, deck) in want {
        commands.entity(model).insert(RideFrame(deck));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::ElevatorKeyframe;

    /// A two-frame pause path at `base_pos` — the same lift both tests below drive.
    fn parked_lift() -> Lift {
        Lift {
            frames: Arc::new(vec![
                ElevatorKeyframe {
                    time_ms: 0,
                    pos: [0.0; 3],
                },
                ElevatorKeyframe {
                    time_ms: 1000,
                    pos: [0.0; 3],
                },
            ]),
            period_ms: 1000,
            base_pos: [10.0, 20.0, 30.0],
            quat: [0.0, 0.0, 0.0, 1.0],
            yaw: 0.5,
        }
    }

    /// A docked lift goes QUIET: `tick_transports` writes pose and visibility on change only
    /// (1473) — the unconditional writes it replaces re-dirtied every docked elevator's whole
    /// subtree (transform + `Visibility` propagation over all its submeshes), map-wide, every
    /// frame. A two-frame pause path samples the identical pose forever, so after the first
    /// arming write nothing may be marked changed.
    #[test]
    fn a_docked_lift_stops_dirtying_its_transform_and_visibility() {
        #[derive(Resource, Default)]
        struct Dirty {
            transforms: usize,
            visibilities: usize,
        }
        fn spy(
            t: Query<Entity, (Changed<Transform>, With<Transport>)>,
            v: Query<Entity, (Changed<Visibility>, With<Transport>)>,
            mut out: ResMut<Dirty>,
        ) {
            out.transforms = t.iter().count();
            out.visibilities = v.iter().count();
        }
        let mut app = App::new();
        app.insert_resource(CurrentMap(0))
            .init_resource::<Dirty>()
            .add_systems(Update, (tick_transports, spy).chain());
        app.world_mut().spawn((
            Guid(0xF110_0000_0000_0001),
            Transport {
                drive: Drive::Lift(parked_lift()),
                was_moving: false,
            },
            TransportAnchor {
                progress_ms: 0,
                at: Instant::now(),
            },
            Transform::default(),
            Visibility::default(),
        ));
        app.update(); // arming frame: the first pose write lands (and spawn reads as Changed)
        for _ in 0..3 {
            app.update();
        }
        let dirty = app.world().resource::<Dirty>();
        assert_eq!(
            (dirty.transforms, dirty.visibilities),
            (0, 0),
            "a docked lift must stop dirtying its transform and visibility"
        );
    }

    /// **A lift is never hidden by the off-map term, whatever `CurrentMap` says** (decision 1654).
    /// The off-map hide is the boat's: a ferry mid-cycle on the other continent's leg. 0611 gave
    /// the lift a `map` stamped from `CurrentMap` at arm time — and `arm_transports` runs a stage
    /// *before* `player::wire_in` writes that resource, so at login every type-11 on the map armed
    /// against the startup seed and the term hid all of them for the session (Thunder Bluff; B168).
    /// Driving the tick on a map the arm never saw is exactly that shape, and the car must show.
    #[test]
    fn a_lift_is_visible_on_whatever_map_the_viewer_is_on() {
        let mut app = App::new();
        // Alterac Valley — a real 1.12 map that carries no type-11 GO at all, so it is a map no
        // lift could ever have been stamped with. The point is that the value cannot matter.
        app.insert_resource(CurrentMap(30))
            .add_systems(Update, tick_transports);
        let lift = app
            .world_mut()
            .spawn((
                Guid(0xF110_0000_0000_0002),
                Transport {
                    drive: Drive::Lift(parked_lift()),
                    was_moving: false,
                },
                TransportAnchor {
                    progress_ms: 0,
                    at: Instant::now(),
                },
                Transform::default(),
                // As the net apply spawns an anchored transport: hidden until its first ticked
                // pose. If the tick declines to unhide it, the car is solid and invisible.
                Visibility::Hidden,
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(lift),
            Some(&Visibility::Inherited),
            "the tick must unhide a lift on the viewer's map, not judge it against a stamped one"
        );
        // And the pose it wrote is the car's own — base + its (zero) local offset, in Bevy axes.
        assert_eq!(
            app.world().get::<Transform>(lift).map(|t| t.translation),
            Some(benilla_assets::coords::wow_to_bevy([10.0, 20.0, 30.0])),
        );
    }
}
