//! The shared movement vocabulary: the `MovementInfo` body (+ its `JumpInfo` ballistic tail and
//! `TransportPose` rider tail) every `MSG_MOVE_*` relay carries, its flag bits, and the parse — read by
//! the update-object living block and [`super`]'s relay arms, written by [`super::client`]'s outbound
//! movement.

use std::io::{self, Read};

use crate::wire::{read_f32_le, read_u32_le, read_u64_le, Vector3d};

// MovementFlags bits, shared by the living [`MovementBlock`](super::MovementBlock) (in
// `update_object`) and the client [`MovementInfo`] parse ([`read_movement_info`]) below.
// ON_TRANSPORT is 1.12's bit 25 (vmangos `MovementInfo.h:56` `MOVEFLAG_ONTRANSPORT = 0x02000000`)
// — NOT the TBC-era 0x200, which in vanilla is `MOVEFLAG_UNUSED10`. The original transcription
// carried 0x200; caught 2026-07-17 against the vmangos source when riding (0438 phase 2) landed.
pub(super) const MOVEMENT_FLAG_ON_TRANSPORT: u32 = 0x0200_0000;
pub(super) const MOVEMENT_FLAG_JUMPING: u32 = 0x2000;
pub(super) const MOVEMENT_FLAG_SWIMMING: u32 = 0x20_0000;
pub(super) const MOVEMENT_FLAG_SPLINE_ENABLED: u32 = 0x40_0000;
pub(super) const MOVEMENT_FLAG_SPLINE_ELEVATION: u32 = 0x400_0000;

/// Read a wire `MovementInfo` — the body shared by every `MSG_MOVE_*` (and the teleport ack). Surfaces
/// `flags`/`position`/`orientation`/`timestamp`/`fall_time`, the **transport pose** ([`TransportPose`],
/// present iff `MOVEFLAG_ON_TRANSPORT` — this is how a boarded rider's `MSG_MOVE_*` heartbeat carries
/// its local frame, decision 0438 "Riding is the mover's platform frame"), the **swim pitch**
/// ([`Self::pitch`], present iff `MOVEFLAG_SWIMMING`), and the **jump tail** ([`JumpInfo`], so an
/// observer can replay a jump arc); the spline-elevation tail is parsed to stay aligned but discarded.
///
/// VERIFIED byte-for-byte against vmangos `MovementInfo::Read` (build 1.12.1): note 1.12 has **no**
/// transport-time field after the transport pose — that was added in TBC. (The `update_object`
/// movement-block transport read, proven against `wow_world_messages`, omits it for the same reason.)
/// The **swim pitch** is a lone `f32` after the (optional) transport pose and before `fallTime`, gated on
/// `MOVEFLAG_SWIMMING`. `fallTime` is a `u32` and the jump tail (gated on `MOVEFLAG_JUMPING`) is `zspeed,
/// cosAngle, sinAngle, xyspeed`, after `fallTime` and before the spline-elevation float.
pub(super) fn read_movement_info(r: &mut impl Read) -> io::Result<MovementInfo> {
    let flags = read_u32_le(r)?;
    let timestamp = read_u32_le(r)?;
    let position = Vector3d::read(r)?;
    let orientation = read_f32_le(r)?;
    let transport = if flags & MOVEMENT_FLAG_ON_TRANSPORT != 0 {
        Some(TransportPose {
            // A FULL u64, not packed: vmangos `data >> t_guid` resolves to the plain
            // `ObjectGuid` operator (`ObjectGuid.cpp:180`, `buf.read<uint64>()`); the packed
            // reader is a distinct operator this path never uses.
            guid: read_u64_le(r)?,
            pos: Vector3d::read(r)?,
            orientation: read_f32_le(r)?,
        })
    } else {
        None
    };
    let pitch = if flags & MOVEMENT_FLAG_SWIMMING != 0 {
        read_f32_le(r)?
    } else {
        0.0
    };
    let fall_time = read_u32_le(r)?;
    let jump = if flags & MOVEMENT_FLAG_JUMPING != 0 {
        Some(JumpInfo {
            zspeed: read_f32_le(r)?,
            cos_angle: read_f32_le(r)?,
            sin_angle: read_f32_le(r)?,
            xy_speed: read_f32_le(r)?,
        })
    } else {
        None
    };
    if flags & MOVEMENT_FLAG_SPLINE_ELEVATION != 0 {
        let _ = read_f32_le(r)?;
    }
    Ok(MovementInfo {
        flags,
        timestamp,
        position,
        orientation,
        transport,
        pitch,
        fall_time,
        jump,
    })
}

// --- shared movement vocabulary (read by the arms above, written by `client`) ---------------------

/// Which of a mover's six movement speeds a force-speed-change packet addresses — the wire family's
/// shared axis (each kind is its own SMSG/CMSG opcode pair; see [`super::opcode`]'s force-speed
/// block). Order matches the `LIVING` block's speed array and vmangos `UnitMoveType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedKind {
    Walk,
    Run,
    RunBack,
    Swim,
    SwimBack,
    TurnRate,
}

impl SpeedKind {
    /// The `CMSG_FORCE_*_SPEED_CHANGE_ACK` opcode answering this kind's SMSG.
    pub fn ack_opcode(self) -> u16 {
        use super::opcode;
        match self {
            SpeedKind::Walk => opcode::CMSG_FORCE_WALK_SPEED_CHANGE_ACK,
            SpeedKind::Run => opcode::CMSG_FORCE_RUN_SPEED_CHANGE_ACK,
            SpeedKind::RunBack => opcode::CMSG_FORCE_RUN_BACK_SPEED_CHANGE_ACK,
            SpeedKind::Swim => opcode::CMSG_FORCE_SWIM_SPEED_CHANGE_ACK,
            SpeedKind::SwimBack => opcode::CMSG_FORCE_SWIM_BACK_SPEED_CHANGE_ACK,
            SpeedKind::TurnRate => opcode::CMSG_FORCE_TURN_RATE_CHANGE_ACK,
        }
    }
}

/// **A granted mover *mode*** — one of the four the server hands the controlling client, each a
/// single `MOVEMENTFLAGS` bit that changes how the mover behaves rather than where it is going
/// (decision 0866). Sibling of [`SpeedKind`]: same shape (a mode per opcode pair, an ack per mode),
/// same reason for existing — the server treats the set as one family (`IsFlagAckOpcode`,
/// `MovementChangeType`), so we do too, and a fifth mode is a variant rather than a new lane.
///
/// The bit values are 1.12's, VERIFIED vmangos `Objects/MovementInfo.h:25-62`. What each one *does*
/// is the client's own business — the server only grants it — and all four are byte-verified in the
/// reference's CMovement setter cluster (`0x7c7280`–`0x7c7370`, wow-re `system/collision`):
///
/// | mode | bit | reference effect | granted by |
/// |---|---|---|---|
/// | [`Root`](Self::Root) | `0x0000_1000` | translation dies; the fall stops. **Turning stays live** — `StartTurn 0x7c6c50` has no root gate. | root/stun/death |
/// | [`WaterWalk`](Self::WaterWalk) | `0x1000_0000` | the liquid surface becomes walkable ground | `SPELL_AURA_WATER_WALK` (104) |
/// | [`FeatherFall`](Self::FeatherFall) | `0x2000_0000` | terminal fall velocity drops to 7 yd/s from 60.148 (`0x7c5d20`) | `SPELL_AURA_FEATHER_FALL` (105) |
/// | [`Hover`](Self::Hover) | `0x4000_0000` | ground contact rises by 1.0 yd (walk resolver `0x6367b0`) | `SPELL_AURA_HOVER` (106) |
///
/// **Levitate (1706) grants three of them at once** — feather fall + hover + water walk — which is
/// what makes them one system rather than four coincidences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMode {
    Root,
    WaterWalk,
    FeatherFall,
    Hover,
}

impl MoveMode {
    /// This mode's `MOVEMENTFLAGS` bit.
    pub fn flag(self) -> u32 {
        match self {
            MoveMode::Root => 0x0000_1000,
            MoveMode::WaterWalk => 0x1000_0000,
            MoveMode::FeatherFall => 0x2000_0000,
            MoveMode::Hover => 0x4000_0000,
        }
    }

    /// The `CMSG_*_ACK` opcode answering this mode's SMSG. Root is the only mode whose two
    /// directions ack on *different* opcodes (vmangos routes both to `HandleMoveRootAck`).
    pub fn ack_opcode(self, apply: bool) -> u16 {
        use super::opcode;
        match (self, apply) {
            (MoveMode::Root, true) => opcode::CMSG_FORCE_MOVE_ROOT_ACK,
            (MoveMode::Root, false) => opcode::CMSG_FORCE_MOVE_UNROOT_ACK,
            (MoveMode::WaterWalk, _) => opcode::CMSG_MOVE_WATER_WALK_ACK,
            (MoveMode::FeatherFall, _) => opcode::CMSG_MOVE_FEATHER_FALL_ACK,
            (MoveMode::Hover, _) => opcode::CMSG_MOVE_HOVER_ACK,
        }
    }

    /// Whether the ack body carries the trailing `u32 apply` dword. Root's does **not**: it lands on
    /// vmangos's `HandleMoveRootAck` (which infers apply from the opcode), while the other three land
    /// on `HandleMovementFlagChangeToggleAck`, which reads it (`Server/Packets/Movement.cpp:38-59`).
    pub fn ack_carries_apply(self) -> bool {
        !matches!(self, MoveMode::Root)
    }
}

/// One jump's ballistic launch parameters — the conditional `MovementInfo` tail present iff the
/// `MOVEFLAG_JUMPING` (0x2000) flag is set. VERIFIED byte-for-byte against vmangos `MovementInfo::Read`
/// (build 1.12.1): wire order is `zspeed, cosAngle, sinAngle, xyspeed` (note cos *before* sin). The
/// horizontal launch velocity is `(cos_angle, sin_angle) · xy_speed` in **world XY** (absolute, frozen
/// at take-off) and `zspeed` is the constant take-off vertical speed (+Z up); the resulting arc is
/// `pos += horiz·t`, `z += zspeed·t − ½·g·t²` (the same `g = 19.291105` the controller uses). This is
/// what lets a receiver replay a jump as one ballistic event between packets rather than snapping its
/// height at the packet rate (decision 0053). Corroborated by vmangos `Unit.cpp` `ExtrapolateMovement`,
/// which integrates exactly this arc from the same fields.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JumpInfo {
    /// Take-off vertical speed (yd/s), constant for the whole arc, **down-positive**: the real 1.12.1
    /// client sends `-7.955547` for a *rising* jump (VERIFIED, vanilla-sniffs `dwarf_rogue_dun_morogh`).
    /// The receiver's up-speed is therefore `-zspeed` (decision 0054).
    pub zspeed: f32,
    /// Cosine of the horizontal launch heading (the X component of the unit launch direction, world XY).
    pub cos_angle: f32,
    /// Sine of the horizontal launch heading (the Y component, world XY).
    pub sin_angle: f32,
    /// Horizontal launch speed (yd/s); the frozen ground speed at take-off.
    pub xy_speed: f32,
}

/// A rider's pose **relative to the transport it's on** — the `MOVEFLAG_ON_TRANSPORT` (0x0200_0000)
/// conditional tail carried by both a `LIVING` update-object movement block (`MovementBlock::transport`,
/// in `update_object`) and a `MSG_MOVE_*` relay's `MovementInfo` ([`Self::transport`]): `[u64
/// transport guid][local x, y, z][local o]`. VERIFIED byte-for-byte against vmangos `MovementInfo::Read`
/// (build 1.12.1) — the guid is a FULL u64 (never packed here), and 1.12 carries **no**
/// transport-time field after this (added TBC).
/// `pos`/`orientation` are in the transport's own local frame (decision 0438 "Riding is the mover's
/// platform frame"): a consumer composes `world = transport_matrix × local` off the named transport's
/// live pose each frame, never treats these as raw world coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportPose {
    /// The transport GameObject's guid (`HIGH_MO_TRANSPORT` or `HIGH_TRANSPORT` — [`crate::guid`]).
    pub guid: u64,
    pub pos: Vector3d,
    pub orientation: f32,
}

/// A wire `MovementInfo` — the body shared by every `MSG_MOVE_*` (both directions) and the teleport
/// ack. benilla sets the base directional/turn/walk flags, `JUMPING` while airborne, `SWIMMING`
/// while in the water, and `ON_TRANSPORT` (+ the [`TransportPose`] tail) while riding a boat/zepp
/// (decision 0438 phase 2). The transport, swim-pitch, and jump tails are conditional
/// outbound, gated on their flags. Inbound, every conditional tail is parsed (see
/// [`read_movement_info`]) — the transport pose into [`Self::transport`], the swim pitch into
/// [`Self::pitch`], the jump tail into [`Self::jump`], and the spline-elevation float to stay aligned
/// only (no consumer needs it).
pub struct MovementInfo {
    pub flags: u32,
    pub timestamp: u32,
    pub position: Vector3d,
    pub orientation: f32,
    /// The sender's pose on a transport — `Some` iff `flags & MOVEMENT_FLAG_ON_TRANSPORT`, carried
    /// right after `orientation` and before the swim-pitch tail (VERIFIED order vs vmangos
    /// `MovementInfo::Read`). This is how another rider's `MSG_MOVE_*` heartbeat tells us they're on a
    /// boat/elevator and where, in that transport's local frame.
    pub transport: Option<TransportPose>,
    /// The **swim pitch** (radians, +up) — the conditional `f32` tail present iff `flags &
    /// MOVEMENT_FLAG_SWIMMING`, carried after the orientation/transport pose and before `fall_time`
    /// (VERIFIED order vs vmangos `MovementInfo::Read`). It is the vertical angle of the swim heading, so
    /// observers can pitch a swimming body up/down the way the local client does. `0.0` (level) when not
    /// swimming — the serializer only emits it while the `SWIMMING` flag is set, so a zero here on a
    /// non-swimming packet costs nothing.
    pub pitch: f32,
    /// Milliseconds airborne — a **`u32`** on the wire (VERIFIED vmangos `MovementInfo::fallTime`; it
    /// was read+written as an f32 while discarded, harmless then but wrong once consumed). The
    /// receiver's ballistic clock: current vertical speed is `zspeed − g·(fall_time/1000)`.
    pub fall_time: u32,
    /// The jump tail — `Some` iff `flags & MOVEMENT_FLAG_JUMPING`. The launch params an observer
    /// replays the arc from.
    pub jump: Option<JumpInfo>,
}

impl MovementInfo {
    pub(super) fn write(&self, w: &mut Vec<u8>) {
        w.extend_from_slice(&self.flags.to_le_bytes());
        w.extend_from_slice(&self.timestamp.to_le_bytes());
        w.extend_from_slice(&self.position.x.to_le_bytes());
        w.extend_from_slice(&self.position.y.to_le_bytes());
        w.extend_from_slice(&self.position.z.to_le_bytes());
        w.extend_from_slice(&self.orientation.to_le_bytes());
        // The transport tail rides iff ON_TRANSPORT is set — right after `orientation`, before the
        // swim pitch, matching vmangos `MovementInfo::Read` exactly: `[u64 guid][local x,y,z][local
        // o]` (the guid is a FULL u64 — see `read_movement_info`). Flag and tail must travel
        // together or the server's read desyncs; the caller sets both from one rider state
        // (decision 0438 "Riding is the mover's platform frame").
        if self.flags & MOVEMENT_FLAG_ON_TRANSPORT != 0 {
            let t = self.transport.unwrap_or(TransportPose {
                guid: 0,
                pos: Vector3d {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                orientation: 0.0,
            });
            w.extend_from_slice(&t.guid.to_le_bytes());
            w.extend_from_slice(&t.pos.x.to_le_bytes());
            w.extend_from_slice(&t.pos.y.to_le_bytes());
            w.extend_from_slice(&t.pos.z.to_le_bytes());
            w.extend_from_slice(&t.orientation.to_le_bytes());
        }
        // The swim-pitch tail rides iff SWIMMING is set — after the transport pose and before
        // `fall_time`, matching `read_movement_info`'s gate and order exactly: setting the flag
        // without serializing the pitch (or vice-versa) desyncs the server's `MovementInfo::Read`,
        // the reason this bit was masked off the wire until now.
        if self.flags & MOVEMENT_FLAG_SWIMMING != 0 {
            w.extend_from_slice(&self.pitch.to_le_bytes());
        }
        // fall_time follows the transport + swim-pitch tails.
        w.extend_from_slice(&self.fall_time.to_le_bytes());
        // The jump tail rides iff JUMPING is set — matching the parser's gate exactly: setting the flag
        // without serializing the tail (or vice-versa) desyncs the server's `MovementInfo::Read`. Order
        // matches vmangos: zspeed, cosAngle, sinAngle, xyspeed.
        if self.flags & MOVEMENT_FLAG_JUMPING != 0 {
            let j = self.jump.unwrap_or_default();
            w.extend_from_slice(&j.zspeed.to_le_bytes());
            w.extend_from_slice(&j.cos_angle.to_le_bytes());
            w.extend_from_slice(&j.sin_angle.to_le_bytes());
            w.extend_from_slice(&j.xy_speed.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Vector3d;

    fn info(flags: u32) -> MovementInfo {
        MovementInfo {
            flags,
            timestamp: 12345,
            position: Vector3d {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            orientation: 0.5,
            transport: None,
            pitch: -0.7,
            fall_time: 42,
            jump: None,
        }
    }

    /// The swim-pitch tail round-trips byte-exactly through `write` → `read_movement_info` when
    /// SWIMMING is set: the reason the flag was masked off the wire (an unwritten tail desyncing the
    /// server's `MovementInfo::Read`) is closed only if write and read agree on gate + order.
    #[test]
    fn swim_pitch_round_trips_when_swimming() {
        let mut bytes = Vec::new();
        info(MOVEMENT_FLAG_SWIMMING).write(&mut bytes);
        let back = read_movement_info(&mut &bytes[..]).expect("parse");
        assert_eq!(back.flags, MOVEMENT_FLAG_SWIMMING);
        assert_eq!(back.pitch, -0.7, "the swim pitch survives the round-trip");
        assert_eq!(back.orientation, 0.5);
        assert_eq!(
            back.fall_time, 42,
            "fall_time still lands after the pitch tail"
        );
    }

    /// The transport tail round-trips byte-exactly through `write` → `read_movement_info` when
    /// ON_TRANSPORT is set: `[u64 guid][x,y,z][o]` right after `orientation` (vmangos
    /// `MovementInfo::Read`/`Write` order), 24 bytes exactly — a packed guid or the TBC 0x200
    /// flag value would both shift `fall_time` and desync the server.
    #[test]
    fn transport_tail_round_trips_when_riding() {
        let mut riding = info(MOVEMENT_FLAG_ON_TRANSPORT);
        riding.transport = Some(TransportPose {
            guid: 0x1FC0_0000_0000_2B10, // a HIGH_MO_TRANSPORT-shaped guid
            pos: Vector3d {
                x: -5.5,
                y: 2.25,
                z: 8.0,
            },
            orientation: 1.5,
        });
        let mut bytes = Vec::new();
        riding.write(&mut bytes);
        let mut walking = Vec::new();
        info(0).write(&mut walking);
        assert_eq!(
            bytes.len() - walking.len(),
            24,
            "the transport tail adds exactly u64 + 4×f32"
        );
        let back = read_movement_info(&mut &bytes[..]).expect("parse");
        let t = back.transport.expect("transport pose survives");
        assert_eq!(t.guid, 0x1FC0_0000_0000_2B10);
        assert_eq!((t.pos.x, t.pos.y, t.pos.z), (-5.5, 2.25, 8.0));
        assert_eq!(t.orientation, 1.5);
        assert_eq!(back.fall_time, 42, "fall_time lands after the tail");
    }

    /// Non-swimming packets carry NO pitch tail — the four pitch bytes are absent, so the wire stays
    /// byte-identical to before this tail existed (and `fall_time` reads from the right offset).
    #[test]
    fn no_pitch_tail_when_not_swimming() {
        let mut swimming = Vec::new();
        info(MOVEMENT_FLAG_SWIMMING).write(&mut swimming);
        let mut dry = Vec::new();
        info(0).write(&mut dry);
        assert_eq!(
            swimming.len() - dry.len(),
            4,
            "the swim pitch adds exactly one f32 to the packet, and only when swimming"
        );
        let back = read_movement_info(&mut &dry[..]).expect("parse");
        assert_eq!(back.pitch, 0.0, "a dry packet parses a level (0) pitch");
        assert_eq!(back.fall_time, 42);
    }
}
