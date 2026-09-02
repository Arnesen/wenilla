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
/// `MoveSplineFlag::Flying` — and in vmangos 1.12 that bit alone *is* `Mask_CatmullRom`
/// (`MoveSplineFlag.h:48,77`), the same split [`crate::ServerPacket::MonsterMove`] reads: set ⇒ a 3-D
/// flight path that keeps its own Z, clear ⇒ a ground walk whose Z the client re-derives from terrain.
const SPLINE_FLAG_FLYING: u32 = 0x200;
/// `SPLINEFLAG_RUNMODE` — see [`super::super::monster_move`]'s copy for why its *absence* is what
/// matters: a spline without it forces walk mode on for the unit it moves.
const SPLINE_FLAG_RUNMODE: u32 = 0x100;
/// `MoveSplineFlag::Cyclic` (`MoveSplineFlag.h:59`) — the path loops back on itself forever.
const SPLINE_FLAG_CYCLIC: u32 = 0x10_0000;

/// The **live spline a unit is already riding** when it streams into view — the `LIVING` block's
/// `MOVEFLAG_SPLINE_ENABLED` tail (vmangos `Object.cpp:535` → `PacketBuilder::WriteCreate`,
/// `packet_builder.cpp:152`). Without it, a creature that was walking when we first saw it stands
/// frozen at its create pose until the server's *next* `SMSG_MONSTER_MOVE` — which then teleports it
/// to wherever it actually got to (decision 0708).
///
/// [`Self::path`] is the **travel-order polyline**, the same shape `MonsterMove` decodes to. The wire
/// carries the server's raw internal control array (`MoveSpline::getPath()` = `Spline::getPoints()`),
/// which is *not* that: vmangos builds every spline — linear ones included — through
/// `SplineBase::InitCatmullRom` ("we should use catmullrom initializer even for linear mode!",
/// `spline.cpp:52`), so the array is `[phantom, p₀, …, pₙ, tail]` whose walkable range is
/// `index_lo = 1 ..= index_hi = len − 2` (`spline.cpp:238-270`): the phantom head is `p₀` extrapolated
/// one segment *backwards*, and the tail duplicates the destination (or, cyclic, closes the loop).
/// We hand over exactly that range.
#[derive(Debug, Clone)]
pub struct CreateSpline {
    /// The full travel-order polyline in raw WoW coords: `[start, …waypoints…, endpoint]`.
    pub path: Vec<[f32; 3]>,
    /// The server's spline id (`MoveSpline::GetId`), as on `MonsterMove`.
    pub id: u32,
    /// Milliseconds of this spline the server has **already ridden** (`MoveSpline::timePassed`): the
    /// unit is `time_passed_ms / duration_ms` of the way along `path`, so a client that starts the
    /// ride at `now − time_passed_ms` lands exactly where the server has it.
    pub time_passed_ms: u32,
    /// The whole path's duration in ms (`MoveSpline::Duration` = the spline's length, which vmangos
    /// initialises in *time* units at one constant velocity — `CommonInitializer`,
    /// `MoveSpline.cpp:125-135`). Time is therefore uniform in arc length along the whole polyline,
    /// which is what makes the `time_passed` fraction above exact.
    pub duration_ms: u32,
    /// `MoveSplineFlag::Flying`: a 3-D flight path (keep the spline's Z); clear for a ground walk.
    pub flying: bool,
    /// `MoveSplineFlag::Cyclic` — the server loops this path forever. The app rides it once; a looping
    /// follow is unbuilt (the same gap `MonsterMove`'s cyclic paths have).
    pub cyclic: bool,
    /// `MoveSplineFlag::Runmode` — and its absence forces walk mode on (decision 1758).
    pub run_mode: bool,
}

/// A `LIVING` block's live mover state: the `MOVEMENTFLAGS` word and the swim pitch that rides it.
/// See [`MovementBlock::mover`] for why both are surfaced rather than parsed and dropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoverState {
    /// The raw `MOVEMENTFLAGS` word (`CMovement+0x40`) — direction bits, swim, falling, the granted
    /// modes.
    pub flags: u32,
    /// The **swim pitch** (radians, +up), present on the wire iff `MOVEFLAG_SWIMMING`; `0.0` (level)
    /// otherwise.
    pub pitch: f32,
}

/// The decoded fields a movement block carries: the pose (from its `LIVING`/`HAS_POSITION` flag),
/// for a `LIVING` block the unit's 6 movement speeds, its live [`CreateSpline`] and (if
/// `ON_TRANSPORT`) its rider pose, and (if `UPDATE_FLAG_TRANSPORT`) a transport GameObject's path
/// progress. The rest of the block is parsed and discarded to stay aligned.
pub struct MovementBlock {
    pub position: Option<(Vector3d, f32)>,
    /// A `LIVING` block's **live `MOVEMENTFLAGS` word** and its **swim pitch** — the mover state this
    /// unit is *already in* at the moment it streams into view. `None` for a `HAS_POSITION`-only
    /// block (a GameObject has no mover).
    ///
    /// **The reference applies both, byte-VERIFIED** (wow-5875-re §5,
    /// `system/collision/scratch/create-block-swim-pitch.md`). The create-block apply `0x619320`
    /// reaches `0x618c30`, which merges the block's flags into `CMovement+0x40` under the same
    /// `0x75a07dff` mask a relayed `MSG_MOVE_*` uses (`0x618df3`; `SWIMMING`, `FORWARD` and
    /// `BACKWARD` are all inside it) and then calls the pose commit `0x7c6420`, whose
    /// `fld [edx+0x34]` / `fst [ecx+0x20]` copies **this pitch** into the mover pitch field
    /// unconditionally, alongside the position and facing. So a unit that streams in mid-swim is
    /// pitched on its **first drawn frame** — the apply runs before `0x613e10` has even built the
    /// model or registered its per-frame render callback.
    ///
    /// Dropping the pair is why a player who came into view mid-swim rendered *level* until their
    /// next relay packet: the body-pitch render law reads exactly these two values and had neither.
    ///
    /// The pitch is `0.0` (level) whenever `MOVEFLAG_SWIMMING` is clear, and that is the
    /// reference's own value, not our convenience: the block field is explicitly zeroed at
    /// `0x47ec4a` on the non-swimming leg, so the unconditional commit stores a real 0.0 rather
    /// than stale memory.
    pub mover: Option<MoverState>,
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
    /// The path this unit is **already walking** at create time (`MOVEFLAG_SPLINE_ENABLED`) — see
    /// [`CreateSpline`]. `None` for a unit standing still, and for every non-`LIVING` block.
    pub spline: Option<CreateSpline>,
}

impl MovementBlock {
    pub(super) fn read(r: &mut impl Read) -> io::Result<Self> {
        let update_flag = read_u8(r)?;
        let mut position = None;
        let mut mover = None;
        let mut speeds = None;
        let mut transport = None;
        let mut transport_progress = None;
        let mut spline = None;

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
            // The swim-pitch tail — a lone `f32` present iff `MOVEFLAG_SWIMMING`, the same gate
            // (and the same position in the layout) the `MSG_MOVE_*` body uses.
            let pitch = if flags & MOVEMENT_FLAG_SWIMMING != 0 {
                read_f32_le(r)?
            } else {
                0.0
            };
            mover = Some(MoverState { flags, pitch });
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
                // The dictated final facing, in vmangos's own angle → target → point order
                // (`packet_builder.cpp:162-167`). Parsed for alignment only: a unit still riding a
                // path faces its travel tangent every frame, exactly as `MonsterMove`'s facing snap is
                // overwritten while a path runs (`net::apply::objects::monster_move`).
                if spline_flags & SPLINE_FLAG_FINAL_ANGLE != 0 {
                    let _ = read_f32_le(r)?;
                } else if spline_flags & SPLINE_FLAG_FINAL_TARGET != 0 {
                    let _ = read_u64_le(r)?;
                } else if spline_flags & SPLINE_FLAG_FINAL_POINT != 0 {
                    let _ = Vector3d::read(r)?;
                }
                let time_passed_ms = read_u32_le(r)?;
                let duration_ms = read_u32_le(r)?;
                let id = read_u32_le(r)?;
                let amount_of_nodes = read_u32_le(r)?;
                let mut nodes = Vec::with_capacity(amount_of_nodes.min(0xFFFF) as usize);
                for _ in 0..amount_of_nodes {
                    let v = Vector3d::read(r)?;
                    nodes.push([v.x, v.y, v.z]);
                }
                // The final destination trails the array; it is `path`'s last real point again
                // (`MoveSpline::FinalDestination` = `getPoint(last())`), or zero on a cyclic path.
                let _final_node = Vector3d::read(r)?;
                // Drop the server's two virtual control points — see [`CreateSpline`]. Fewer than
                // four nodes cannot hold a two-point path, so there is nothing to ride.
                spline = (nodes.len() >= 4).then(|| CreateSpline {
                    path: nodes[1..nodes.len() - 1].to_vec(),
                    id,
                    time_passed_ms,
                    duration_ms,
                    flying: spline_flags & SPLINE_FLAG_FLYING != 0,
                    cyclic: spline_flags & SPLINE_FLAG_CYCLIC != 0,
                    run_mode: spline_flags & SPLINE_FLAG_RUNMODE != 0,
                });
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
            mover,
            speeds,
            transport_progress,
            transport,
            spline,
        })
    }
}
