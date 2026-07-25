//! Our own mover's `WorldWriter` sends — the self-movement stream (`MSG_MOVE_*`) and the acks the
//! server demands of a player mover (speed change, near-teleport, worldport, root/unroot,
//! water-walk, spline-done). Bodies in [`crate::messages`]'s `movement`/`move_*`/`teleport_ack`/
//! `force_speed_ack` builders; the `MovementInfo` stamping is
//! [`crate::world::movement`]'s. Split out of `writer/mod.rs` (decision 0636), the family the
//! writer type itself was originally built for.
//!
//! What unites these beyond the opcode range: every one of them carries a full `MovementInfo`
//! snapshot (or is the empty echo of one), and every ack is **mandatory** — un-acked, the server
//! either force-resolves the change on a timeout and flags its anticheat, or completes the
//! teleport ~20 s late.

use anyhow::Result;

use crate::messages::{self, opcode, JumpInfo, TransportPose};
use crate::world::movement::{client_uptime_ms, movement_info};

use super::WorldWriter;

impl WorldWriter {
    /// Send one self-movement packet: a `MSG_MOVE_*` `opcode` carrying a `MovementInfo` with the given
    /// `flags` + pose. The caller (the controller) chooses the opcode per movement-axis transition
    /// (start/stop forward/back/strafe/turn), the periodic heartbeat, and the facing update — exactly as
    /// the real client does — and the `flags` it passes are the live CMovement bits the server relays to
    /// nearby players. `flags` must only set bits whose `MovementInfo` tail we serialize — the base
    /// directional/turn/walk bits, `JUMPING` (with its `jump` tail), `SWIMMING` (with its `pitch`
    /// tail), and `ON_TRANSPORT` (with its `transport` local-frame tail — decision 0438 phase 2).
    #[allow(clippy::too_many_arguments)]
    pub fn send_movement(
        &mut self,
        opcode: u16,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        pitch: f32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    ) -> Result<()> {
        let mut info = movement_info(pos, orientation, flags);
        // Each conditional tail is gated on its flag by the serializer, so the flag and the value must
        // agree (they share `flags`): `SWIMMING` ⇒ the swim pitch, `JUMPING` ⇒ the ballistic launch
        // tail, `ON_TRANSPORT` ⇒ the rider's local pose.
        info.pitch = pitch;
        info.fall_time = fall_time;
        info.jump = jump;
        info.transport = transport;
        self.send(opcode, &messages::movement(&info))
    }

    /// Acknowledge that a server-authored spline (Charge/knockback/taxi — an `SMSG_MONSTER_MOVE`
    /// addressed to our own guid) finished: `CMSG_MOVE_SPLINE_DONE` with a `MovementInfo` at the
    /// ride's endpoint and the `spline_id` we were driven by. The server sets `SplineDonePending` for
    /// a player mover and validates this against its newest spline id, then relocates us and
    /// re-broadcasts a stop/heartbeat to observers — so it must be sent, at rest, once the ride ends.
    pub fn move_spline_done(
        &mut self,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        spline_id: u32,
    ) -> Result<()> {
        let info = movement_info(pos, orientation, flags);
        self.send(
            opcode::CMSG_MOVE_SPLINE_DONE,
            &messages::move_spline_done(&info, spline_id),
        )
    }

    /// Echo a cross-map worldport ack: confirms `SMSG_NEW_WORLD` so the server resumes its object
    /// stream on the new continent (`MSG_MOVE_WORLDPORT_ACK` has an empty body).
    pub fn worldport_ack(&mut self) -> Result<()> {
        self.send(opcode::MSG_MOVE_WORLDPORT_ACK, &[])
    }

    /// Answer a `SMSG_FORCE_*_SPEED_CHANGE` (`CMSG_FORCE_*_SPEED_CHANGE_ACK`, picked by `kind`):
    /// echo the mover `guid` + `counter` + the exact `speed` the server sent, carrying our live
    /// `MovementInfo` (same field set as [`Self::send_movement`] — the server relocates us to it).
    /// Mandatory: unacked, the server force-resolves the change after ~4 s and flags its anticheat.
    #[allow(clippy::too_many_arguments)]
    pub fn force_speed_change_ack(
        &mut self,
        kind: messages::SpeedKind,
        guid: u64,
        counter: u32,
        speed: f32,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        pitch: f32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    ) -> Result<()> {
        let mut info = movement_info(pos, orientation, flags);
        info.pitch = pitch;
        info.fall_time = fall_time;
        info.jump = jump;
        info.transport = transport;
        self.send(
            kind.ack_opcode(),
            &messages::force_speed_ack(guid, counter, &info, speed),
        )
    }

    /// Echo a same-map teleport ack so the server completes the near-teleport **immediately**
    /// (relocates us + streams the surrounding objects). Without a valid ack the teleport only
    /// finishes on a ~20s server-side fallback, so the destination's objects appear ~20s late.
    ///
    /// vanilla 1.12 / vmangos expect a **full 8-byte guid** for this opcode (the lone movement opcode
    /// that does — the `MovementInfo` ones are correctly packed). With a packed guid the server's
    /// `ByteBuffer` overruns and drops the packet (confirmed by a vmangos capture: opcode `0xC7` →
    /// `ByteBufferException`, then the teleport completing 20s later).
    pub fn teleport_ack(&mut self, guid: u64, counter: u32) -> Result<()> {
        self.send(
            opcode::MSG_MOVE_TELEPORT_ACK,
            &messages::teleport_ack(guid, counter, client_uptime_ms()),
        )
    }

    /// Acknowledge a server root/unroot on our mover (`SMSG_FORCE_MOVE_[UN]ROOT` → the matching
    /// `CMSG_FORCE_MOVE_[UN]ROOT_ACK`): full guid + the echoed counter + our current
    /// `MovementInfo`. Un-acked, the change never reaches observers; a wrong/zero counter trips
    /// the server's cheat log (`HandleMoveRootAck`). While rooted, `flags` must not carry moving
    /// bits (vmangos `MovementInfo.h`: moving flags with `MOVEFLAG_ROOT` freeze the real client).
    pub fn move_root_ack(
        &mut self,
        guid: u64,
        counter: u32,
        rooted: bool,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
    ) -> Result<()> {
        let info = movement_info(pos, orientation, flags);
        self.send(
            if rooted {
                opcode::CMSG_FORCE_MOVE_ROOT_ACK
            } else {
                opcode::CMSG_FORCE_MOVE_UNROOT_ACK
            },
            &messages::move_flag_ack(guid, counter, &info, None),
        )
    }

    /// Acknowledge a water-walk grant/removal on our mover (`SMSG_MOVE_WATER_WALK`/`LAND_WALK` →
    /// `CMSG_MOVE_WATER_WALK_ACK`, one ack opcode for both directions): the root ack's shape plus
    /// the trailing `u32 apply` the server's `MoveFlagChangeAck` reader expects.
    pub fn water_walk_ack(
        &mut self,
        guid: u64,
        counter: u32,
        on: bool,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
    ) -> Result<()> {
        let info = movement_info(pos, orientation, flags);
        self.send(
            opcode::CMSG_MOVE_WATER_WALK_ACK,
            &messages::move_flag_ack(guid, counter, &info, Some(on)),
        )
    }
}
