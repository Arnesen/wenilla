//! Our own mover's `WorldWriter` sends — the self-movement stream (`MSG_MOVE_*`) and the acks the
//! server demands of a player mover (speed change, near-teleport, worldport, the granted
//! movement-mode family, spline-done). Bodies in [`crate::messages`]'s `movement`/`move_*`/`teleport_ack`/
//! `force_speed_ack` builders; the `MovementInfo` stamping is
//! [`crate::world::movement`]'s. Split out of `writer/mod.rs` (decision 0636), the family the
//! writer type itself was originally built for.
//!
//! What unites these beyond the opcode range: every one of them carries a full `MovementInfo`
//! snapshot (or is the empty echo of one), and every ack is **mandatory** — un-acked, the server
//! either force-resolves the change on a timeout and flags its anticheat, or completes the
//! teleport ~20 s late.

use anyhow::Result;

use crate::messages::{self, opcode, JumpInfo, MoveMode, TransportPose};
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

    /// Claim `guid` as the unit whose movement we are sending (`CMSG_SET_ACTIVE_MOVER`, a full u64).
    ///
    /// **The mover handshake is client-driven — no server packet sets it.** The server states who
    /// we *may* drive (`SMSG_CLIENT_CONTROL_UPDATE`) and then waits; until this reply lands,
    /// `Player::GetConfirmedMover` resolves nothing for the new mover and **every** `MSG_MOVE_*` we
    /// send for it is discarded (vmangos `MovementHandler.cpp:844-884`, which also rejects a claim
    /// on a unit it never handed us). Login makes this claim for our own body; possession simply
    /// re-makes it for somebody else's.
    pub fn set_active_mover(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_SET_ACTIVE_MOVER, &messages::full_guid(guid))
    }

    /// Vote on the far-sight view the server has already chosen (`CMSG_FAR_SIGHT`, one `u8`).
    ///
    /// The real client sends this from exactly two sites, both inside its far-sight engage/release
    /// function `0x5ee290`: `1` when the view attaches (`5ee3cb`/`5ee3dd`), `0` when it releases
    /// (`5ee4f3`/`5ee504`). It names no object — the server resolves that from `PLAYER_FARSIGHT`,
    /// which only the server writes.
    pub fn far_sight(&mut self, engage: bool) -> Result<()> {
        self.send(opcode::CMSG_FAR_SIGHT, &[u8::from(engage)])
    }

    /// Give up `guid` as our mover (`CMSG_MOVE_NOT_ACTIVE_MOVER`): the full u64 guid being
    /// released, then a `MovementInfo` at its parting pose.
    ///
    /// The pose is the point. vmangos clears its confirmed-mover state and **re-broadcasts a stop
    /// under the old guid** built from what we send here (`MovementHandler.cpp:886-965`), so
    /// skipping it strands every observer on that unit's last relayed pose — a possessed creature
    /// left sliding where the possession ended.
    pub fn move_not_active_mover(
        &mut self,
        guid: u64,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        fall_time: u32,
    ) -> Result<()> {
        let mut info = movement_info(pos, orientation, flags);
        info.fall_time = fall_time;
        let mut body = messages::full_guid(guid);
        body.extend_from_slice(&messages::movement(&info));
        self.send(opcode::CMSG_MOVE_NOT_ACTIVE_MOVER, &body)
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

    /// **Acknowledge a granted mover mode** — root, water-walk, feather-fall or hover (the ack'd
    /// family; decision 0866): full guid + the echoed counter + our current `MovementInfo`, plus the
    /// trailing `u32 apply` for every mode but root ([`MoveMode::ack_carries_apply`]). Un-acked, the
    /// server never applies the change and observers never see it; a wrong/zero counter trips its
    /// cheat log (`HandleMoveRootAck`).
    ///
    /// `flags` is the caller's live wire word and must already carry the applied mode's bit. Two
    /// server rules constrain it: an apply-ack without the bit is a KICK for root
    /// (`HandleMoveRootAck:715`), and **moving bits must never accompany `MOVEFLAG_ROOT`** — they
    /// freeze the real client (vmangos `MovementInfo.h`) and trip `CHEAT_TYPE_ROOT_MOVE`. Turn bits
    /// are not moving bits (`MOVEFLAG_MASK_MOVING` excludes them) and are fine alongside root.
    ///
    /// `pose` is the mover's live `(position, orientation)` — the pair `World::self_pose` returns.
    pub fn move_mode_ack(
        &mut self,
        guid: u64,
        counter: u32,
        mode: MoveMode,
        apply: bool,
        flags: u32,
        pose: ([f32; 3], f32),
    ) -> Result<()> {
        let info = movement_info(pose.0, pose.1, flags);
        let trailing = mode.ack_carries_apply().then_some(apply);
        self.send(
            mode.ack_opcode(apply),
            &messages::move_flag_ack(guid, counter, &info, trailing),
        )
    }

    /// **Acknowledge the knockback we are now flying** (`CMSG_MOVE_KNOCK_BACK_ACK`, decision 1702):
    /// full guid + the echoed counter + our `MovementInfo` at the moment of launch, whose jump tail
    /// is the server's own `launch` quad echoed back.
    ///
    /// The echo is not politeness — it is the server's whole validation. `flags` must carry
    /// `MOVEFLAG_JUMPING`, and `launch` must be bit-for-bit what arrived (vmangos matches all four
    /// floats within `0.01`); miss either and `FindPendingMovementKnockbackChange` fails, the ack is
    /// logged as `OnWrongAckData`, and no observer ever sees the knockback — the server builds their
    /// `MSG_MOVE_KNOCK_BACK` from the `MovementInfo` sent here. `fall_time` is the launch's fresh
    /// clock (0).
    pub fn knock_back_ack(
        &mut self,
        guid: u64,
        counter: u32,
        launch: JumpInfo,
        flags: u32,
        pose: ([f32; 3], f32),
        transport: Option<TransportPose>,
    ) -> Result<()> {
        let mut info = movement_info(pose.0, pose.1, flags);
        info.fall_time = 0;
        info.jump = Some(launch);
        info.transport = transport;
        self.send(
            opcode::CMSG_MOVE_KNOCK_BACK_ACK,
            &messages::knock_back_ack(guid, counter, &info),
        )
    }
}
