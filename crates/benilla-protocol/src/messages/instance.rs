//! The instance/raid **lockout message** family — the six server packets the 1.12 client turns
//! into a `CHAT_MSG_SYSTEM` line or into its own lockout bookkeeping, plus the one thing the
//! player can ask for (`CMSG_RESET_INSTANCES`). Decision 1748.
//!
//! | opcode | direction | body | what the client does with it |
//! |---|---|---|---|
//! | `SMSG_INSTANCE_SAVE_CREATED` 0x2cb | in | `u32` flag | chat line `INSTANCE_SAVED` |
//! | `SMSG_RAID_INSTANCE_MESSAGE` 0x2fa | in | `u32` type, `u32` mapId, `u32` secsToReset | chat line, one of four |
//! | `CMSG_RESET_INSTANCES` 0x31d | out | empty | — |
//! | `SMSG_INSTANCE_RESET` 0x31e | in | `u32` mapId | chat line `INSTANCE_RESET_SUCCESS` + clears the last-instance latch |
//! | `SMSG_INSTANCE_RESET_FAILED` 0x31f | in | `u32` reason, `u32` mapId | chat line, one of three |
//! | `SMSG_UPDATE_LAST_INSTANCE` 0x320 | in | `u32` mapId | records the last dungeon we were in |
//! | `SMSG_UPDATE_INSTANCE_OWNERSHIP` 0x32b | in | `u32` flag | the "do I hold any permanent bind" latch |
//!
//! **The client side is VERIFIED at the bytes** (WoW.exe build 5875): the six handlers are
//! registered together at `0x498680`-`0x4986cf` (`0x49e1c0` 0x2fa, `0x49e470` 0x31e, `0x49e540`
//! 0x31f, `0x49e670` 0x320, `0x49e6c0` 0x32b) with the save-created one registered apart, in the
//! chat-spam-filter TU, at `0x4e7e48` (`0x4e7e60`). Every one reads its fields with
//! `CDataStore::GetInt32 0x418e30` in the order below, so the layouts here are the client's own
//! read order, not an inference from the server. The server side is VERIFIED vmangos
//! `Server/Packets/Misc.{h,cpp}` (`RaidInstanceMessage`, `InstanceReset`, `InstanceResetFailed`,
//! `UpdateLastInstance`, `UpdateInstanceOwnership`) + `Maps/Map.cpp:2288,2388` for the
//! save-created flag, and the two agree field for field.
//!
//! **No map NAME is ever on the wire** — every one of these carries a `Map.dbc` id and the client
//! looks the display name up itself (`[0xc0daa8][id]`, field `+0x10` = `MapName_Lang[locale]`),
//! falling back to the id printed through `"%d"` when the row is missing. Ours resolves through
//! `MapCatalog::name` with the same fallback.

use std::io;

use crate::wire::read_u32_le;

/// `SMSG_RAID_INSTANCE_MESSAGE`'s `type` — which of the four warning templates the line uses
/// (VERIFIED: the client's `switch (type - 1)` jump table at `0x49e45c`, four entries; the same
/// numbering vmangos writes in `Objects/Player.h:568-573`).
///
/// **Type 5 (`RAID_INSTANCE_EXPIRED` in later clients) and type 0 print nothing**: the dispatch is
/// `dec eax; cmp eax,3; ja <return>`, so anything outside 1..=4 leaves the handler silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaidInstanceWarning {
    /// 1 — `RAID_INSTANCE_WARNING_HOURS`, filled with `resetTime / 3600`.
    Hours,
    /// 2 — `RAID_INSTANCE_WARNING_MIN`, filled with `resetTime / 60`.
    Minutes,
    /// 3 — `RAID_INSTANCE_WARNING_MIN_SOON`, filled with `resetTime / 60`.
    MinutesSoon,
    /// 4 — `RAID_INSTANCE_WELCOME`, filled with the `d`/`h`/`m` breakdown of `resetTime`.
    Welcome,
}

impl RaidInstanceWarning {
    /// The wire `type` → the template, or `None` for the values the reference's jump table drops
    /// (0 and ≥ 5). A dropped type is not an error: the packet is well-formed, the client just
    /// has no line for it.
    pub fn from_wire(ty: u32) -> Option<Self> {
        match ty {
            1 => Some(Self::Hours),
            2 => Some(Self::Minutes),
            3 => Some(Self::MinutesSoon),
            4 => Some(Self::Welcome),
            _ => None,
        }
    }

    /// The GlobalStrings token the reference passes to `GetText` for this template (the four
    /// `mov ecx, <string>` at `0x49e276`/`0x49e2e5`/`0x49e354`/`0x49e40b`).
    pub fn token(self) -> &'static str {
        match self {
            Self::Hours => "RAID_INSTANCE_WARNING_HOURS",
            Self::Minutes => "RAID_INSTANCE_WARNING_MIN",
            Self::MinutesSoon => "RAID_INSTANCE_WARNING_MIN_SOON",
            Self::Welcome => "RAID_INSTANCE_WELCOME",
        }
    }
}

/// `SMSG_RAID_INSTANCE_MESSAGE` — a raid lockout's welcome/countdown line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaidInstanceMessage {
    /// The raw wire type. Kept raw rather than pre-resolved so a type the reference drops still
    /// round-trips through the event layer (and so a golden can assert the drop).
    pub message_type: u32,
    /// `Map.dbc` id of the instance the warning is about.
    pub map: u32,
    /// **Seconds remaining** until the reset, not a timestamp (vmangos passes
    /// `resetTime` = a remaining duration into `SendInstanceResetWarning`).
    pub reset: u32,
}

/// `SMSG_INSTANCE_RESET_FAILED`'s `reason` — which of the three refusal templates the line uses
/// (VERIFIED: the client's `sub eax,0; je / dec eax; je / dec eax; jne` ladder at `0x49e5b7`;
/// vmangos `Maps/MapPersistentStateMgr.h:270-276`, whose fourth value `_SILENTLY` = 3 is
/// deliberately outside the ladder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceResetFailure {
    /// 0 — `INSTANCE_RESET_FAILED`: players still inside.
    General,
    /// 1 — `INSTANCE_RESET_FAILED_OFFLINE`: someone in the party is offline.
    Offline,
    /// 2 — `INSTANCE_RESET_FAILED_ZONING`: someone in the party is zoning in.
    Zoning,
}

impl InstanceResetFailure {
    /// The wire `reason` → the template, or `None` for ≥ 3 (`INSTANCERESET_FAIL_SILENTLY` and
    /// above). The reference *falls through* those to its chat call with an **uninitialized**
    /// stack buffer — a real 1.12 bug we do not reproduce: silence is what the server means by
    /// them, and printing a garbage line is not fidelity to an intent, only to an accident.
    pub fn from_wire(reason: u32) -> Option<Self> {
        match reason {
            0 => Some(Self::General),
            1 => Some(Self::Offline),
            2 => Some(Self::Zoning),
            _ => None,
        }
    }

    /// The GlobalStrings token (the three `mov ecx, <string>` at `0x49e61b`/`0x49e5f6`/`0x49e5d1`).
    pub fn token(self) -> &'static str {
        match self {
            Self::General => "INSTANCE_RESET_FAILED",
            Self::Offline => "INSTANCE_RESET_FAILED_OFFLINE",
            Self::Zoning => "INSTANCE_RESET_FAILED_ZONING",
        }
    }
}

/// `SMSG_INSTANCE_RESET_FAILED` — the group leader's reset was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceResetFailed {
    /// The raw wire reason (see [`InstanceResetFailure::from_wire`]).
    pub reason: u32,
    /// `Map.dbc` id of the instance that would not reset.
    pub map: u32,
}

/// Read `SMSG_RAID_INSTANCE_MESSAGE`: `u32 type`, `u32 mapId`, `u32 secondsUntilReset` — the
/// client's own read order at `0x49e1cd`/`0x49e1d8`/`0x49e1e3`.
pub(super) fn read_raid_instance_message(r: &mut &[u8]) -> io::Result<RaidInstanceMessage> {
    Ok(RaidInstanceMessage {
        message_type: read_u32_le(r)?,
        map: read_u32_le(r)?,
        reset: read_u32_le(r)?,
    })
}

/// Read `SMSG_INSTANCE_RESET_FAILED`: `u32 reason`, `u32 mapId` (`0x49e54d`/`0x49e558`).
pub(super) fn read_instance_reset_failed(r: &mut &[u8]) -> io::Result<InstanceResetFailed> {
    Ok(InstanceResetFailed {
        reason: read_u32_le(r)?,
        map: read_u32_le(r)?,
    })
}

/// Read a body that is one bare `u32` — `SMSG_INSTANCE_RESET`'s map id (`0x49e481`),
/// `SMSG_UPDATE_LAST_INSTANCE`'s map id (`0x49e676`), `SMSG_UPDATE_INSTANCE_OWNERSHIP`'s flag
/// (`0x49e6c6`) and `SMSG_INSTANCE_SAVE_CREATED`'s flag (`0x4e7e6c`) are all exactly this.
pub(super) fn read_u32_body(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// Body of `CMSG_RESET_INSTANCES` (VERIFIED both ways: the client's `ResetInstances` binding
/// `0x48a6b0` builds a `CDataStore` and sends opcode `0x31d` without writing a field; vmangos
/// `HandleResetInstancesOpcode` reads nothing): empty.
pub fn reset_instances() -> Vec<u8> {
    Vec::new()
}
