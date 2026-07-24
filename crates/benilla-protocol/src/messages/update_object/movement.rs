use std::io::{self, Read};

use crate::messages::movement::{
    TransportPose, MOVEMENT_FLAG_JUMPING, MOVEMENT_FLAG_ON_TRANSPORT,
    MOVEMENT_FLAG_SPLINE_ELEVATION, MOVEMENT_FLAG_SPLINE_ENABLED, MOVEMENT_FLAG_SWIMMING,
};
use crate::wire::{read_f32_le, read_packed_guid, read_u32_le, read_u64_le, read_u8, Vector3d};

/// Object class (the `TypeId` on a create packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Object,
    Item,
    Container,
    Unit,
    Player,
    GameObject,
    DynamicObject,
    Corpse,
}

impl ObjectType {
    pub(super) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Item,
            2 => Self::Container,
            3 => Self::Unit,
            4 => Self::Player,
            5 => Self::GameObject,
            6 => Self::DynamicObject,
            7 => Self::Corpse,
            _ => Self::Object,
        }
    }
}

// MovementBlock update_flag bits (UpdateObject-only; the shared MOVEMENT_FLAG_* live in `super`).
const UPDATE_FLAG_TRANSPORT: u8 = 0x02;
const UPDATE_FLAG_MELEE_ATTACKING: u8 = 0x04;
const UPDATE_FLAG_HIGH_GUID: u8 = 0x08;
const UPDATE_FLAG_ALL: u8 = 0x10;
const UPDATE_FLAG_LIVING: u8 = 0x20;
const UPDATE_FLAG_HAS_POSITION: u8 = 0x40;
// SplineFlag final-destination bits (checked angle → target → point).
const SPLINE_FLAG_FINAL_POINT: u32 = 0x1_0000;
const SPLINE_FLAG_FINAL_TARGET: u32 = 0x2_0000;
const SPLINE_FLAG_FINAL_ANGLE: u32 = 0x4_0000;

/// The decoded fields a movement block carries: the pose (from its `LIVING`/`HAS_POSITION` flag),
/// for a `LIVING` block the unit's 6 movement speeds and (if `ON_TRANSPORT`) its rider pose, and (if
/// `UPDATE_FLAG_TRANSPORT`) a transport GameObject's path progress. The rest of the block is parsed and
/// discarded to stay aligned.
pub struct MovementBlock {
    pub position: Option<(Vector3d, f32)>,
    /// A `LIVING` block's 6 movement speeds (yd/s) in wire order `[walk, run, run_back, swim, swim_back,
    /// turn_rate]`; `None` for a `HAS_POSITION`-only block (GameObjects, which don't move under their
    /// own power). The animation selector keys walk-vs-run on `walk` (boundary 2× it — RF-0057); the
    /// net bridge extrapolates a remote mover between packets at `run`/`run_back`/`swim` + `turn_rate`.
    pub speeds: Option<[f32; 6]>,
    /// A transport GameObject's (`UPDATE_FLAG_TRANSPORT`, 0x02) path progress: the server's ms clock in
    /// the path domain (vmangos `Object.cpp:590-605` — `GenericTransport::GetPathProgress()`, the same
    /// `m_pathProgress` `ShipTransport::Update`/`CalculateSegmentPos` key off, `Transports/Transport.cpp:
    /// 285-301`). `None` for every non-transport object: vmangos raises `UPDATEFLAG_TRANSPORT` only in
    /// the transport GO paths — `ShipTransport`'s ctor (`Transport.cpp:39`) and the type-11 elevator
    /// branch of `GameObject::Create` (`GameObject.cpp:246`); units/players never set it
    /// (`Unit.cpp:88`). This is decision 0438's
    /// **cycle anchor**: `anchor = this value`, `t₀ = Instant::now()` on create, `progress = anchor +
    /// elapsed_ms` thereafter.
    pub transport_progress: Option<u32>,
    /// A `LIVING` block's rider pose on a transport (`MOVEFLAG_ON_TRANSPORT`, 0x0200_0000) — `Some` when this
    /// unit/player is standing on a boat/zeppelin/elevator, its position local to that transport's frame
    /// (decision 0438 "Riding is the mover's platform frame" — an observed rider carried this way
    /// re-anchors through the transport's live matrix each frame, never treating `pos` as world-space).
    pub transport: Option<TransportPose>,
}

impl MovementBlock {
    pub(super) fn read(r: &mut impl Read) -> io::Result<Self> {
        let update_flag = read_u8(r)?;
        let mut position = None;
        let mut speeds = None;
        let mut transport = None;
        let mut transport_progress = None;

        if update_flag & UPDATE_FLAG_LIVING != 0 {
            let flags = read_u32_le(r)?;
            let _timestamp = read_u32_le(r)?;
            let living_position = Vector3d::read(r)?;
            let living_orientation = read_f32_le(r)?;
            position = Some((living_position, living_orientation));

            if flags & MOVEMENT_FLAG_ON_TRANSPORT != 0 {
                // A FULL u64, not packed: the LIVING block serializes through the same vmangos
                // `MovementInfo::Write` as the MSG_MOVE relays (`Object.cpp:524` `*data << m`),
                // whose `data << t_guid` is the plain-u64 `ObjectGuid` operator.
                transport = Some(TransportPose {
                    guid: read_u64_le(r)?,
                    pos: Vector3d::read(r)?,
                    orientation: read_f32_le(r)?,
                });
            }
            if flags & MOVEMENT_FLAG_SWIMMING != 0 {
                let _pitch = read_f32_le(r)?;
            }
            let _fall_time = read_f32_le(r)?;
            if flags & MOVEMENT_FLAG_JUMPING != 0 {
                for _ in 0..4 {
                    let _ = read_f32_le(r)?; // z_speed, cos_angle, sin_angle, xy_speed
                }
            }
            if flags & MOVEMENT_FLAG_SPLINE_ELEVATION != 0 {
                let _ = read_f32_le(r)?;
            }
            // The 6 movement speeds (RF-0058 order): [0]=walk, [1]=run, [2]=run-back, [3]=swim,
            // [4]=swim-back, [5]=turn-rate. The animation selector keys walk-vs-run on the walk speed
            // (boundary = 2× it — RF-0057); the net bridge extrapolates a remote mover with the rest.
            let mut s = [0.0f32; 6];
            for slot in &mut s {
                *slot = read_f32_le(r)?;
            }
            speeds = Some(s);
            if flags & MOVEMENT_FLAG_SPLINE_ENABLED != 0 {
                let spline_flags = read_u32_le(r)?;
                if spline_flags & SPLINE_FLAG_FINAL_ANGLE != 0 {
                    let _ = read_f32_le(r)?;
                } else if spline_flags & SPLINE_FLAG_FINAL_TARGET != 0 {
                    let _ = read_u64_le(r)?;
                } else if spline_flags & SPLINE_FLAG_FINAL_POINT != 0 {
                    let _ = Vector3d::read(r)?;
                }
                let _time_passed = read_u32_le(r)?;
                let _duration = read_u32_le(r)?;
                let _id = read_u32_le(r)?;
                let amount_of_nodes = read_u32_le(r)?;
                for _ in 0..amount_of_nodes {
                    let _ = Vector3d::read(r)?;
                }
                let _final_node = Vector3d::read(r)?;
            }
        } else if update_flag & UPDATE_FLAG_HAS_POSITION != 0 {
            let pos = Vector3d::read(r)?;
            let orientation = read_f32_le(r)?;
            position = Some((pos, orientation));
        }

        if update_flag & UPDATE_FLAG_HIGH_GUID != 0 {
            let _ = read_u32_le(r)?;
        }
        if update_flag & UPDATE_FLAG_ALL != 0 {
            let _ = read_u32_le(r)?;
        }
        if update_flag & UPDATE_FLAG_MELEE_ATTACKING != 0 {
            let _ = read_packed_guid(r)?;
        }
        if update_flag & UPDATE_FLAG_TRANSPORT != 0 {
            transport_progress = Some(read_u32_le(r)?);
        }

        Ok(Self {
            position,
            speeds,
            transport_progress,
            transport,
        })
    }
}
