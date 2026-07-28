//! Server-authored movement edges applied to our mover — the inbound mirror of
//! [`super::movement_net`] (which streams our own movement out). One entry point,
//! [`apply_server_moves`], called by [`super::control`] before input integrates: cross-map
//! worldports (incl. the riding-through-the-seam branch, decision 0455), same-map teleports,
//! root/unroot (death — decision 0308), water-walk grants, the one-shot take-control edge, the
//! pre-control forced-speed acks (the controlled branch answers those through the movement
//! stream's own per-frame payload instead — the returned list), and the **bare self-addressed
//! move** ([`apply_self_move`], decision 0725).
//!
//! All but the last carry a **mandatory ack**, which is what made the last one easy to miss: a
//! `MSG_MOVE_*` the server addresses to our own guid arrives with no handshake and owes no answer,
//! and this client used to discard it. The real client applies it — its inbound move path has no
//! mover-guid gate at all — and sends nothing back; the next ordinary heartbeat carries the
//! server's own pose home, which is what makes `.go forward` stick.

use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};

use crate::creature_anim::{move_flags, wrap_pi};
use crate::death::{MoveRootMessage, WaterWalkMessage};
use crate::net::{
    ClientCommand, Guid, MoveKind, NetCommands, SelfMoveMessage, SelfPlayer, SpeedChangeMessage,
    TeleportMessage, WorldportMessage,
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
    self_moves: &mut MessageReader<SelfMoveMessage>,
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
            // …and the physics world under the new position is still the OLD map's until the
            // streamer swaps it, one stage from now. See [`Player::world_stale`].
            player.world_stale = true;
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
        // A far same-map teleport relocates us over ground that has not streamed either — the
        // tiles we were standing on are still the resident ones. See [`Player::world_stale`].
        player.world_stale = true;
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
    // A bare server-authored move for our own mover (decision 0725) — no handshake, no ack. Drained
    // after the teleport arms deliberately: a teleport in the same frame is the larger edge (it
    // swaps maps, holds the settle and owes an ack), so it wins the pose.
    for m in self_moves.read() {
        if player.active {
            apply_self_move(m, player, cam, time, transports);
        }
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
            player.world_stale = true; // nothing is streamed yet at all — see [`Player::world_stale`]
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

/// Merge a server-authored packet's `MOVEMENTFLAGS` into our own — the reference's masked merge
/// (`0x618c30 @0x618deb`: `new = old ^ ((old ^ wire) & 0x75a07dff)`), not an assignment. Pure, so
/// the omission that actually bites is pinned by test: `ON_TRANSPORT` sits **outside** the mask, so
/// a server-authored pose can relocate a rider but never board or deboard them. See
/// [`move_flags::SERVER_AUTHORED`].
pub(super) fn merge_server_flags(local: u32, wire: u32) -> u32 {
    (local & !move_flags::SERVER_AUTHORED) | (wire & move_flags::SERVER_AUTHORED)
}

/// Apply one bare self-addressed `MSG_MOVE_*` — a pose the *server* wrote for our own mover, with
/// no handshake (decision 0725; wow-re `self-addressed-move.md`). `.go forward`/`up`/`relative`,
/// `.cheat fly`/`fixedz` and the movement anticheat's snap-back all land here.
///
/// **A hard snap, and nothing goes back.** The reference writes the wire pose into both its live
/// position cell and its integrator *base* (`0x7c6420`), which is what makes the snap persist —
/// the next frame integrates forward from the server's point instead of rubber-banding off it. For
/// us those are one cell ([`Player::pos`]), so the snap is the whole of it. It sends no ack and
/// opens no suppression window: our ordinary heartbeat then carries the server's own pose home,
/// which is precisely why `.go forward` sticks.
///
/// **Applied inline, not scheduled** — the one place this departs from the reference's shape, and
/// it is a deliberate divergence rather than a shortcut. The reference has a single move machine
/// and routes a self-addressed packet through the same replay chain as a remote's (decisions
/// 0601/0615). Measured against that chain (`net::motion::tests`'
/// `the_chain_paces_a_server_authored_self_move_and_would_hold_an_early_one`) it does **not** always
/// come back due: the first one fires at arrival, but once the chain has stamps to pace against, a
/// packet arriving ahead of the sender's cadence is deliberately *held* — bounded at +1000 ms.
///
/// That holding is the chain's whole purpose and it is right for a remote: replaying a mover on the
/// sender's own spacing is what stops a watched player stuttering between packets. It buys nothing
/// here. Our avatar is not something we extrapolate between packets — there is no motion to
/// de-jitter — so preserving the "cadence" between one GM command and the next would only delay a
/// correction to our own pose behind a queue the controller would have to yield to. We take the
/// snap at arrival. The trigger to revisit is a self-addressed *stream*: `Anticheat.Enable = 1`,
/// whose snap-backs can burst, is the one sender that would produce one.
// The transports query type is `control`'s own param shape passed through, like the caller's.
#[allow(clippy::type_complexity)]
fn apply_self_move(
    m: &SelfMoveMessage,
    player: &mut Player,
    cam: &mut FlyCam,
    time: &Time,
    transports: &Query<
        (&Transform, &Guid),
        (With<Transport>, Without<SelfPlayer>, Without<FlyCam>),
    >,
) {
    let was_falling = player.move_flags & move_flags::FALLING != 0;
    player.move_flags = merge_server_flags(player.move_flags, m.flags);
    let now_falling = player.move_flags & move_flags::FALLING != 0;

    // **Lift the granted mover MODES out of the merged word into typed state** (decision 0726).
    // `move_flags` is our last-streamed wire bookkeeping and is rebuilt from state every frame, so a
    // bit parked there alone would be gone before the mover ever read it; the modes live as fields,
    // the way `rooted` does. The reference needs no such step — it has one `[cmov+0x40]` that is both
    // the state and the wire word — and it has the matching apply anyway: the same inbound merge also
    // runs `0x61a1af → 0x61a230 → SetSwim` (wow-re `swim-transition.md`, "the local unit's server
    // echo"). This pair is GM flight: `.cheat fly` sends SWIMMING + LEVITATING together.
    player.swimming = player.move_flags & move_flags::SWIMMING != 0;
    player.levitating = player.move_flags & move_flags::LEVITATING != 0;
    if player.swimming {
        // A latched swim ends the settle hold — `update_swimming`'s own rule, and it has to be
        // repeated here because that function is exactly what LEVITATING switches off. Without it,
        // `.cheat fly` during a post-teleport settle strands `settling` forever: the swim mover
        // never reaches the walk mover's release gate, and the loading screen waits on `!settling`
        // (the shape of the Booty Bay underwater-login hang).
        player.settling = false;
    }

    player.pos = wow_to_bevy(m.position);
    // Facing turns **rigidly** — aim, rendered body and camera all take the same delta (the
    // transport carry's idiom). The reference writes only the mover's own facing cells; hard-setting
    // the camera the way the teleport arm does would yank the view on every `.cheat fly` toggle,
    // whose heartbeat carries nothing but the server's slightly-stale copy of our own orientation.
    let dyaw = wrap_pi(m.orientation - player.face_yaw);
    player.face_yaw = wrap_pi(player.face_yaw + dyaw);
    player.model_yaw = wrap_pi(player.model_yaw + dyaw);
    cam.yaw += dyaw;
    player.swim_pitch = m.pitch;

    // Riding a deck: `ON_TRANSPORT` is outside the merge mask, so this packet did not deboard us —
    // it moved us **within** the platform frame. Re-anchor the local pose from the boat's live
    // transform, or next frame's carry recomposes the stale one and undoes the snap.
    if let Some(ride) = player.ride.as_mut() {
        if let Ok((boat, _)) = transports.get(ride.entity) {
            ride.local_pos = boat.rotation.inverse() * (player.pos - boat.translation);
            ride.boat_yaw = boat.rotation.to_euler(EulerRot::YXZ).0;
        }
    }

    // The airborne arc. The reference gates the fall tail on the *merged* `FALLING` bit — a data
    // gate, not an identity one — seeding the arc from the wire when it is set (`0x7c6490`) and
    // running the landing reaction when it just cleared.
    match (was_falling, now_falling) {
        (_, true) => {
            let (vel_y, xy) = crate::net::jump_seed(m.jump, m.fall_time);
            let t = m.fall_time as f32 / 1000.0;
            player.airborne_since = Some(time.elapsed_secs() - t);
            player.jump_zspeed = m.jump.map_or(0.0, |j| -j.zspeed);
            player.vel_y = vel_y;
            player.horiz_vel = wow_to_bevy([xy[0], xy[1], 0.0]);
            // Where this arc launched, recovered from the pose it is at now: `z = z₀ + v₀t − ½gt²`.
            player.fall_start_y =
                player.pos.y - (player.jump_zspeed * t - 0.5 * crate::player::GRAVITY * t * t);
            player.fall_far = player.move_flags & move_flags::FALLING_FAR != 0;
            player.airborne_dirs = player.move_flags & move_flags::ANY_MOVE;
        }
        (true, false) => {
            // The arc is over by server decree (`NearLandTo` strips `JUMPING|FALLINGFAR` before it
            // sends). Ended **silently**: our landing report is a fall *height*, and the same packet
            // just moved the body an arbitrary distance, so there is no descent left to measure.
            player.airborne_since = None;
            player.fall_far = false;
            player.vel_y = 0.0;
        }
        (false, false) => {}
    }
    player.wedged = false;
    player.wedge_still = 0;
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

#[cfg(test)]
mod self_move_tests {
    use super::merge_server_flags;
    use crate::creature_anim::move_flags as f;

    /// The merge is not an assignment, and the bit that proves it is `ON_TRANSPORT`: it sits
    /// outside the reference's `0x75a07dff` mask, so a server-authored pose relocates a rider on
    /// the deck without ever boarding or deboarding them. Everything else we model is inside, and
    /// the wire owns it — including `FALLING`, whose clearing is how `.go forward` ends an arc.
    #[test]
    fn a_server_move_owns_the_wire_bits_and_leaves_the_transport_bit_alone() {
        // Riding a boat, running forward. The server says: standing still, falling, not on a boat.
        let local = f::ON_TRANSPORT | f::FORWARD;
        let wire = f::FALLING;
        let merged = merge_server_flags(local, wire);
        assert_eq!(
            merged & f::ON_TRANSPORT,
            f::ON_TRANSPORT,
            "the packet must not deboard a rider — bit 25 is the client's"
        );
        assert_eq!(merged & f::FALLING, f::FALLING, "the wire owns FALLING");
        assert_eq!(merged & f::FORWARD, 0, "and it owns the direction bits too");

        // The other direction: the wire claiming ON_TRANSPORT cannot board us either.
        assert_eq!(merge_server_flags(0, f::ON_TRANSPORT), 0);
    }

    /// **`.cheat fly` survives the merge intact** (decision 0726). vmangos's `Player::SetFly` sends
    /// `LEVITATING | SWIMMING | MOVED | FLYING`; the two we model must both be inside the mask, or
    /// GM flight arrives half-applied — SWIMMING without LEVITATING is a swimmer on dry land that
    /// the water decision clears on the next frame, and LEVITATING without SWIMMING is a walker
    /// whose swim latch has simply been frozen.
    #[test]
    fn the_fly_toggle_arrives_whole() {
        const SET_FLY: u32 = f::LEVITATING | f::SWIMMING | 0x0080_0000 | 0x0100_0000;
        let merged = merge_server_flags(f::FORWARD, SET_FLY);
        assert_eq!(merged & f::LEVITATING, f::LEVITATING);
        assert_eq!(merged & f::SWIMMING, f::SWIMMING);
        // …and `.cheat fly off` (flags 0) takes both away again, which is what lands us.
        assert_eq!(
            merge_server_flags(merged, 0) & (f::LEVITATING | f::SWIMMING),
            0
        );
    }

    /// Every flag benilla models except `ON_TRANSPORT` is inside the mask — a guard against the
    /// mask and our constants drifting apart as new bits get modelled.
    #[test]
    fn every_modelled_flag_but_the_transport_bit_is_server_authored() {
        for (name, bit) in [
            ("LEVITATING", f::LEVITATING),
            ("FORWARD", f::FORWARD),
            ("BACKWARD", f::BACKWARD),
            ("STRAFE_LEFT", f::STRAFE_LEFT),
            ("STRAFE_RIGHT", f::STRAFE_RIGHT),
            ("TURN_LEFT", f::TURN_LEFT),
            ("TURN_RIGHT", f::TURN_RIGHT),
            ("WALK_MODE", f::WALK_MODE),
            ("ROOT", f::ROOT),
            ("FALLING", f::FALLING),
            ("FALLING_FAR", f::FALLING_FAR),
            ("SWIMMING", f::SWIMMING),
            ("WATER_WALKING", f::WATER_WALKING),
        ] {
            assert_eq!(
                bit & f::SERVER_AUTHORED,
                bit,
                "{name} must be inside the mask"
            );
        }
        assert_eq!(f::ON_TRANSPORT & f::SERVER_AUTHORED, 0);
    }
}
