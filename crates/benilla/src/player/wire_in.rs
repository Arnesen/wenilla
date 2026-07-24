//! Server-authored movement edges applied to our mover, each with its mandatory ack — the inbound
//! mirror of [`super::movement_net`] (which streams our own movement out). One entry point,
//! [`apply_server_moves`], called by [`super::control`] before input integrates: cross-map
//! worldports (incl. the riding-through-the-seam branch, decision 0455), same-map teleports,
//! root/unroot (death — decision 0308), water-walk grants, the one-shot take-control edge, and
//! the pre-control forced-speed acks (the controlled branch answers those through the movement
//! stream's own per-frame payload instead — the returned list).

use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};

use crate::creature_anim::{move_flags, wrap_pi};
use crate::death::{MoveRootMessage, WaterWalkMessage};
use crate::net::{
    ClientCommand, Guid, MoveKind, NetCommands, SelfPlayer, SpeedChangeMessage, TeleportMessage,
    WorldportMessage,
};
use crate::transport::Transport;
use crate::world_map::CurrentMap;

use super::camera::FlyCam;
use super::{movement_net, Player, SETTLE_TIMEOUT};

/// Drain this frame's server-authored movement messages, apply them to the mover, and send each
/// ack. Returns the frame's forced-speed changes: pre-control/detached they were acked here with
/// the parked pose; controlled, the caller's movement stream acks them with its live payload.
/// `self_pos` is the streamed self entity's current translation (the take-control snap target).
// One system phase's full input set (the spawner precedent); the transports query type is
// `control`'s own param shape passed through.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn apply_server_moves(
    time: &Time,
    commands: &mut Commands,
    player: &mut Player,
    cam: &mut FlyCam,
    net_cmds: &NetCommands,
    teleports: &mut MessageReader<TeleportMessage>,
    worldports: &mut MessageReader<WorldportMessage>,
    speed_msgs: &mut MessageReader<SpeedChangeMessage>,
    root_msgs: &mut MessageReader<MoveRootMessage>,
    waterwalk_msgs: &mut MessageReader<WaterWalkMessage>,
    transports: &Query<
        (&Transform, &Guid),
        (With<Transport>, Without<SelfPlayer>, Without<FlyCam>),
    >,
    self_pos: Option<Vec3>,
) -> Vec<SpeedChangeMessage> {
    // Cross-map worldport (`.tele Orgrimmar`, initial-login map, a boat crossing the sea): the net
    // bridge surfaced it as a message earlier this frame (WorldStage::Net). Snap the avatar, bump
    // `CurrentMap` so the terrain streamer swaps ADTs on the next frame, and ack if required (the
    // ack unblocks the new map stream).
    for w in worldports.read() {
        let riding = w.transport_entry.is_some() && player.ride.is_some();
        if riding {
            // Riding through the transfer (decision 0455): the pose is BOAT-LOCAL (vmangos
            // `SendNewWorld` sends the rider's `GetTransportPos()`), and the boat entity was
            // spared through the worldport purge — recompose the world pose through its live
            // transform. NO settle hold: the deck is the support and its collider never
            // unloaded — and settling ("held") would drop MOVEFLAG_ONTRANSPORT from the first
            // post-crossing move packet (the 0447 flag law is `ride && !held`), which the
            // server reads as a deboard mid-ocean.
            let ride = player.ride.as_mut().expect("riding checked above");
            ride.local_pos = wow_to_bevy(w.position);
            if let Ok((boat, _)) = transports.get(ride.entity) {
                let boat_yaw = boat.rotation.to_euler(EulerRot::YXZ).0;
                ride.boat_yaw = boat_yaw;
                player.pos = boat.translation + boat.rotation * ride.local_pos;
                // Local wire orientation + boat yaw = world facing (the GetAbsoluteFacing law);
                // carry the body and camera to match, as the deck turn does (rigid for the
                // whole rider — a lone aim carry leaves the body-chase to sweep + shuffle).
                let dyaw = wrap_pi(w.orientation + boat_yaw - player.face_yaw);
                player.face_yaw += dyaw;
                player.model_yaw = wrap_pi(player.model_yaw + dyaw);
                cam.yaw += dyaw;
            } else {
                // The spared boat is gone (shouldn't happen — the spare predicate keys on the
                // ride's own path). Land at the local pose read as world: wrong but bounded;
                // the server's post-ack stream corrects us.
                warn!("worldport: riding but the boat entity is missing — using raw pose");
                player.pos = wow_to_bevy(w.position);
                player.ride = None;
            }
        } else {
            // A transfer the server did NOT carry a transport through (GM `.tele`, dungeon
            // port): world pose — and any ride is stale, the server detached us (without this
            // the next frame's carry would yank the avatar back onto the boat).
            player.ride = None;
            player.pos = wow_to_bevy(w.position);
            player.face_yaw = w.orientation;
            player.model_yaw = w.orientation; // a teleport snaps the body — no chase across a loading screen
            cam.yaw = w.orientation;
            player.settling = true; // hold (gravity off) until the new map's ground streams in
            player.settle_deadline = time.elapsed_secs() + SETTLE_TIMEOUT;
        }
        player.move_flags = 0;
        player.airborne_since = None; // a snap ends any in-progress jump arc (no phantom FALL_LAND)
                                      // `insert_resource` replaces if it exists; terrain_stream watches it for diff vs the
                                      // loaded map. If terrain setup never ran (no `./WoW/`), inserting is harmless.
        commands.insert_resource(CurrentMap(w.map_id));
        if w.needs_ack {
            let _ = net_cmds.0.send(ClientCommand::WorldportAck);
            info!(
                "worldport: mapId {} @ {:?} ({}, acked)",
                w.map_id,
                w.position,
                if riding {
                    "riding, boat-local pose"
                } else {
                    "world pose"
                }
            );
        } else {
            info!(
                "worldport: initial login on mapId {} @ {:?}",
                w.map_id, w.position
            );
        }
    }
    // Same-map teleport (the bridge only emits ours). Snap + echo the ack — without it the server
    // freezes our movement until relog.
    for t in teleports.read() {
        player.pos = wow_to_bevy(t.position);
        player.face_yaw = t.orientation;
        cam.yaw = t.orientation;
        // Stop any in-progress walk — server now sees us at the new spot.
        player.move_flags = 0;
        player.airborne_since = None; // a snap ends any in-progress jump arc (no phantom FALL_LAND)
        player.settling = true; // hold (gravity off) until the destination's ground/buildings load
        player.settle_deadline = time.elapsed_secs() + SETTLE_TIMEOUT;
        // The relocation voids any in-progress self server-ride (the taxi flight-end teleport
        // beats our own spline end by ~latency): `drive_self_ride` takes this flag next frame
        // and drops the ride instead of mirroring the stale flight pose back over this snap
        // (decision 0501 — the 4-yd hover + full-6s settle at every taxi landing).
        player.ride_abort = true;
        let _ = net_cmds.0.send(ClientCommand::TeleportAck {
            guid: t.guid,
            counter: t.counter,
        });
        // After the ack, report our settled position. vmangos refreshes a STATIONARY player's
        // surrounding object visibility only on its lazy relocation timer (~20s observed), but forces
        // an immediate refresh on any received movement packet. Without this, the NPCs/GameObjects at
        // the destination don't appear for ~20s after a teleport (yet a fresh login is instant, because
        // that does a full world-enter) — the real client reports its position, so they show at once.
        let _ = net_cmds.0.send(ClientCommand::Move {
            kind: MoveKind::Stop,
            flags: 0,
            pos: t.position,
            orientation: t.orientation,
            pitch: 0.0, // a Stop clears the flags → not swimming → no pitch tail
            fall_time: 0,
            jump: None,
            transport: None, // flags 0 → no transport tail
        });
        info!(
            "teleport: snapped to {:?}, acked + reported position",
            t.position
        );
    }
    // Server root/unroot on our mover (death/release — decision 0308): apply the change locally,
    // THEN ack with the resulting flags — the real client's shape, and the server's law: a
    // root-apply ack whose MovementInfo lacks MOVEFLAG_ROOT is a KICK (vmangos
    // `HandleMoveRootAck:715-723`, live-verified against the deploy's Movement.log). Moving bits
    // never accompany ROOT (they freeze the real client), so the walk stream parks first.
    for m in root_msgs.read() {
        player.rooted = m.rooted;
        if m.rooted {
            movement_net::park_mover(&net_cmds.0, player);
        }
        let facing = player.face_yaw.rem_euclid(std::f32::consts::TAU);
        let _ = net_cmds.0.send(ClientCommand::MoveRootAck {
            guid: m.guid,
            counter: m.counter,
            rooted: m.rooted,
            flags: if m.rooted { move_flags::ROOT } else { 0 },
            pos: bevy_to_wow(player.pos),
            orientation: facing,
        });
        info!(
            "mover {} (acked)",
            if m.rooted { "rooted" } else { "unrooted" }
        );
    }
    // Water-walk grant/removal (the ghost form): ack with the applied flag (the faithful echo;
    // the toggle-ack path doesn't hard-require it the way the root ack does). The walk-on-water
    // mover regime itself is the swim arc's deferred seam (0308 §8).
    for m in waterwalk_msgs.read() {
        let facing = player.face_yaw.rem_euclid(std::f32::consts::TAU);
        let _ = net_cmds.0.send(ClientCommand::WaterWalkAck {
            guid: m.guid,
            counter: m.counter,
            on: m.on,
            flags: if m.on { move_flags::WATER_WALKING } else { 0 },
            pos: bevy_to_wow(player.pos),
            orientation: facing,
        });
    }

    // Take control once the server first reports our position (the streamed `SelfPlayer` entity,
    // whose transform is already in Bevy space). From here the controller drives that entity
    // directly; the entity renderer attaches its body model (0041) the same way it does for any
    // other player.
    if !player.active {
        if let Some(pos) = self_pos {
            player.pos = pos;
            player.active = true;
            player.settling = true; // settle onto the initial ground once it loads (don't fall through)
            player.settle_deadline = time.elapsed_secs() + SETTLE_TIMEOUT;
            cam.yaw = 0.0;
            cam.pitch = -0.45;
            player.face_yaw = 0.0;
            player.model_yaw = 0.0;
            // The avatar's `MovementState` (the animation selector's motion source) is NOT inserted
            // here: it rides the `SelfPlayer` tag (`net::apply::tag_self_player`), because a
            // cross-map worldport despawns and re-streams this entity while `player.active` stays
            // true — per-entity state attached only on this one-shot edge would be lost on transfer.
            info!(
                "took control of player @ {:?} ('F' toggles free-fly)",
                player.pos
            );
        }
    }

    // Forced speed changes (aura/mount/GM `.modify speed`): the net bridge already applied the new
    // value to our `UnitSpeeds`; the mandatory ack is ours to send, carrying our live wire state
    // (the server relocates us to it — the TeleportMessage pattern). In the controlled branch the
    // ack rides the movement stream's exact per-frame payload (`stream_self_movement`); detached or
    // pre-control, our honest wire state IS the parked pose (flags 0), so answer here directly.
    let speed_acks: Vec<SpeedChangeMessage> = speed_msgs.read().copied().collect();
    if !player.active || player.detached {
        for ack in &speed_acks {
            let _ = net_cmds.0.send(ClientCommand::ForceSpeedAck {
                kind: ack.kind,
                guid: ack.guid,
                counter: ack.counter,
                speed: ack.speed,
                flags: 0,
                pos: bevy_to_wow(player.pos),
                orientation: player.face_yaw.rem_euclid(std::f32::consts::TAU),
                pitch: 0.0,
                fall_time: 0,
                jump: None,
                transport: None, // flags 0 → no transport tail
            });
        }
    }
    speed_acks
}

/// A confirmed `/logout` (decision 0193): drop control so the next login re-takes it from its own
/// streamed `SelfPlayer` — possibly a different character on a different map (the boot path). The
/// avatar entity itself is despawned by the net drain the same frame this message is written.
pub(super) fn release_on_logout(
    mut msgs: MessageReader<crate::net::LoggedOutMessage>,
    mut player: ResMut<Player>,
) {
    if msgs.read().next().is_some() {
        player.active = false;
        player.move_flags = 0;
        player.airborne_since = None;
        player.wedged = false;
        player.wedge_still = 0;
    }
}
