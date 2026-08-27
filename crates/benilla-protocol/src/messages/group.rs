//! Group/party messages — invite/accept/decline/kick/leader/disband, the loot method, the roster
//! push, party command feedback, live member stats for the party/raid frame, minimap pings, raid
//! subgroup management, raid-target icons, and ready checks (opcodes 110/111/114-127, 469,
//! 638-640, 654-655, 754, 801-802 — VERIFIED vmangos `Server/Protocol/Opcodes_1_12_1.h`). CMSG
//! bodies from vmangos `Server/Packets/Group.{h,cpp}` (every `ReadFromWorldPacket`); SMSG layouts
//! from the same file's `AppendBodyTo` writers plus the hand-serialized member-stats builder in
//! `Handlers/GroupHandler.cpp` (line citations inline in each item). `ObjectGuid`'s own
//! `operator<</>>` (`ObjectGuid.cpp:172-183`) is a plain 8-byte `u64` — every guid below is FULL
//! unless explicitly marked PACKED (only [`PartyMemberStatsInfo`]'s subject guid is).

use std::io::{self, Read};

use crate::messages::update_object::power_display_scale;
use crate::wire::{
    read_cstring, read_f32_le, read_packed_guid, read_u16_le, read_u32_le, read_u64_le, read_u8,
};

/// The raid-assistant bit in [`GroupMemberEntry::flags`] / `SMSG_GROUP_LIST`'s own-flags byte —
/// bits 0-2 carry the subgroup index (0-7, raid-only; a party has just one), this bit marks a
/// raid assistant (VERIFIED vmangos `Group.cpp:158,166`: `uint8(itr.group | (itr.assistant ? 0x80
/// : 0))`).
pub const GROUP_MEMBER_ASSISTANT: u8 = 0x80;

/// `GroupMemberStatus` (VERIFIED vmangos `Group/Group.h:102-111`) — the bits of
/// [`GroupMemberEntry::status`] (bit `0x20` is reserved/unused — `MEMBER_STATUS_UNK3`, never set).
pub mod member_status {
    pub const OFFLINE: u8 = 0x00;
    pub const ONLINE: u8 = 0x01;
    pub const PVP: u8 = 0x02;
    pub const DEAD: u8 = 0x04;
    pub const GHOST: u8 = 0x08;
    pub const PVP_FFA: u8 = 0x10;
    pub const AFK: u8 = 0x40;
    pub const DND: u8 = 0x80;
}

/// `PartyOperation` (VERIFIED vmangos `Server/WorldSession.h:94-98`) — the `u32` on
/// `SMSG_PARTY_COMMAND_RESULT` naming which command the result answers.
pub mod party_operation {
    pub const INVITE: u32 = 0;
    pub const LEAVE: u32 = 2;
}

/// `PartyResult` (VERIFIED vmangos `Server/WorldSession.h:100-111`) — the `u32` verdict on
/// `SMSG_PARTY_COMMAND_RESULT`. `ERR_INTERNAL_BATTLEGROUND` (10, "does not exist client-side" per
/// vmangos's own comment) never reaches the wire and is omitted.
pub mod party_result {
    pub const OK: u32 = 0;
    pub const BAD_PLAYER_NAME: u32 = 1;
    pub const TARGET_NOT_IN_GROUP: u32 = 2;
    pub const GROUP_FULL: u32 = 3;
    pub const ALREADY_IN_GROUP: u32 = 4;
    pub const NOT_IN_GROUP: u32 = 5;
    pub const NOT_LEADER: u32 = 6;
    pub const WRONG_FACTION: u32 = 7;
    pub const IGNORING_YOU: u32 = 8;
}

/// One other member row on `SMSG_GROUP_LIST` (VERIFIED vmangos `Server/Packets/Group.h:261-267`,
/// `Group.cpp:161-167`, `Group::SendUpdate`). The recipient's own row never appears in the list —
/// their subgroup/assistant flag rides `SMSG_GROUP_LIST`'s separate `own_flags` byte instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMemberEntry {
    pub name: String,
    pub guid: u64,
    /// [`member_status`] bits (`GetGroupMemberStatus`, `Group.cpp:45-63`).
    pub status: u8,
    /// Subgroup index in bits 0-2; [`GROUP_MEMBER_ASSISTANT`] (`0x80`) set = raid assistant.
    pub flags: u8,
}

/// The loot-method tail `SMSG_GROUP_LIST` appends only when the member list is non-empty
/// (VERIFIED `Group.cpp:170-179`). `threshold` is an `ItemQualities` value (2..4 in practice —
/// uncommon and up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupLootInfo {
    /// `0` free-for-all, `1` round-robin, `2` master, `3` group, `4` need-before-greed (vmangos
    /// `LootMethod`, `Group.h`).
    pub method: u8,
    /// The master looter's guid — `0` unless `method == 2` (master loot).
    pub master: u64,
    pub threshold: u8,
}

/// Read `SMSG_GROUP_LIST` (VERIFIED vmangos `Group.cpp:155-180`, `GroupList::AppendBodyTo`):
/// `u8 groupType` (0 party / 1 raid), `u8 ownGroupAndAssistantFlag`, `u32 memberCount`,
/// `memberCount` × [`GroupMemberEntry`], `u64 leaderGuid`, then — only if `memberCount > 0` — the
/// [`GroupLootInfo`] tail plus a trailing `u8 dungeonDifficulty` (always `0` at 5875, `Group.h:281`
/// — "unused in 1.x"). The "you left the group" shape is the degenerate empty-member-list case:
/// exactly 14 bytes (`0, 0, u32 0, u64 0`), no tail at all.
#[allow(clippy::type_complexity)]
pub(super) fn read_group_list(
    r: &mut &[u8],
) -> io::Result<(u8, u8, Vec<GroupMemberEntry>, u64, Option<GroupLootInfo>)> {
    let group_type = read_u8(r)?;
    let own_flags = read_u8(r)?;
    let count = read_u32_le(r)?;
    let mut members = Vec::with_capacity(count as usize);
    for _ in 0..count {
        members.push(GroupMemberEntry {
            name: read_cstring(r)?,
            guid: read_u64_le(r)?,
            status: read_u8(r)?,
            flags: read_u8(r)?,
        });
    }
    let leader = read_u64_le(r)?;
    let loot = if count > 0 {
        let method = read_u8(r)?;
        let master = read_u64_le(r)?;
        let threshold = read_u8(r)?;
        let _dungeon_difficulty = read_u8(r)?; // always 0 at 5875 (Group.h:281)
        Some(GroupLootInfo {
            method,
            master,
            threshold,
        })
    } else {
        None
    };
    Ok((group_type, own_flags, members, leader, loot))
}

/// Read `SMSG_GROUP_INVITE` (VERIFIED vmangos `Group.cpp:107-110`,
/// `GroupInviteNotification::AppendBodyTo`): one cstring, the inviter's name.
pub(super) fn read_group_invite(r: &mut impl Read) -> io::Result<String> {
    read_cstring(r)
}

/// Read `SMSG_GROUP_DECLINE` (VERIFIED vmangos `Group.cpp:112-115`,
/// `GroupDeclineNotification::AppendBodyTo`): one cstring, the declining player's name.
pub(super) fn read_group_decline(r: &mut impl Read) -> io::Result<String> {
    read_cstring(r)
}

/// Read `SMSG_GROUP_SET_LEADER` (VERIFIED vmangos `Group.cpp:150-153`,
/// `GroupSetLeaderNotification::AppendBodyTo`): one cstring, the new leader's name.
pub(super) fn read_group_set_leader(r: &mut impl Read) -> io::Result<String> {
    read_cstring(r)
}

/// Read `SMSG_PARTY_COMMAND_RESULT` (VERIFIED vmangos `Group.cpp:100-105`,
/// `PartyCommandResult::AppendBodyTo`): `u32 operation` ([`party_operation`]), `cstring
/// memberName` (may be empty — e.g. the ignoring-you refusal names no one, `Handlers/
/// GroupHandler.cpp:466`), `u32 result` ([`party_result`]).
pub(super) fn read_party_command_result(r: &mut impl Read) -> io::Result<(u32, String, u32)> {
    Ok((read_u32_le(r)?, read_cstring(r)?, read_u32_le(r)?))
}

/// `GROUP_UPDATE_FLAG_*` (VERIFIED vmangos `Group/Group.h:124-151`) — the bits of the leading `u32`
/// mask on `SMSG_PARTY_MEMBER_STATS`/`_FULL`, selecting which fields follow, each in ascending bit
/// order (see [`read_party_member_stats`]).
pub mod party_member_mask {
    pub const STATUS: u32 = 0x0000_0001;
    pub const CUR_HP: u32 = 0x0000_0002;
    pub const MAX_HP: u32 = 0x0000_0004;
    pub const POWER_TYPE: u32 = 0x0000_0008;
    pub const CUR_POWER: u32 = 0x0000_0010;
    pub const MAX_POWER: u32 = 0x0000_0020;
    pub const LEVEL: u32 = 0x0000_0040;
    pub const ZONE: u32 = 0x0000_0080;
    pub const POSITION: u32 = 0x0000_0100;
    pub const AURAS: u32 = 0x0000_0200;
    pub const AURAS_NEGATIVE: u32 = 0x0000_0400;
    pub const PET_GUID: u32 = 0x0000_0800;
    pub const PET_NAME: u32 = 0x0000_1000;
    pub const PET_MODEL_ID: u32 = 0x0000_2000;
    pub const PET_CUR_HP: u32 = 0x0000_4000;
    pub const PET_MAX_HP: u32 = 0x0000_8000;
    pub const PET_POWER_TYPE: u32 = 0x0001_0000;
    pub const PET_CUR_POWER: u32 = 0x0002_0000;
    pub const PET_MAX_POWER: u32 = 0x0004_0000;
    pub const PET_AURAS: u32 = 0x0008_0000;
    pub const PET_AURAS_NEGATIVE: u32 = 0x0010_0000;
}

/// One `SMSG_PARTY_MEMBER_STATS`/`_FULL` payload — both opcodes decode through the same field
/// layout (the caller picks `full` off which opcode arrived, see [`super::ServerPacket::name`]'s
/// arm): a packed guid, a `u32` field mask ([`party_member_mask`]), then only the fields whose bit
/// is set — `None` otherwise (VERIFIED vmangos `Handlers/GroupHandler.cpp:590-742`,
/// `BuildPartyMemberStatsPacket`). The plain (delta) opcode's mask names only what *changed*; the
/// `_FULL` opcode (our own [`request_party_member_stats`] ask, or the offline-miss reply) sets
/// every bit the target has data for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartyMemberStatsInfo {
    pub status: Option<u8>,
    pub cur_hp: Option<u16>,
    pub max_hp: Option<u16>,
    pub power_type: Option<u8>,
    pub cur_power: Option<u16>,
    pub max_power: Option<u16>,
    pub level: Option<u16>,
    pub zone: Option<u16>,
    /// Raw WoW `(x, y)`, truncated to `i16` on the wire (vmangos casts the live float position,
    /// `GroupHandler.cpp:623`).
    pub position: Option<(i16, i16)>,
    /// Active buff spell ids, ascending bit order — the wire's `u32` position-mask prefix is
    /// decoded away (no consumer needs the raw aura slot, only which buffs are up).
    pub auras: Option<Vec<u16>>,
    /// Active debuff spell ids, same convention as [`Self::auras`] (the wire mask here is `u16`).
    pub auras_negative: Option<Vec<u16>>,
    pub pet_guid: Option<u64>,
    pub pet_name: Option<String>,
    pub pet_model_id: Option<u16>,
    pub pet_cur_hp: Option<u16>,
    pub pet_max_hp: Option<u16>,
    pub pet_power_type: Option<u8>,
    pub pet_cur_power: Option<u16>,
    pub pet_max_power: Option<u16>,
    pub pet_auras: Option<Vec<u16>>,
    pub pet_auras_negative: Option<Vec<u16>>,
}

impl PartyMemberStatsInfo {
    /// **The `1/1` placeholder** — the record a member *new to the roster* starts life with
    /// (VERIFIED wow-re `ui/scratch/party-oor-stats-and-portrait-law.md` §2.2: the GROUP_LIST slot
    /// writer `0x4e82d0` zero-fills the record and then stores `1` into `+0x0a`/`+0x0c`/`+0x0e`/
    /// `+0x10` at `0x4e833d`-`0x4e8357`). It is what an out-of-range member you have never seen
    /// shows until their first stats packet lands: a *full* bar, not an empty one. `status` carries
    /// the roster's own online bit, which is the one field the writer takes from its caller.
    pub fn placeholder(online: bool) -> Self {
        Self {
            status: Some(u8::from(online)),
            cur_hp: Some(1),
            max_hp: Some(1),
            cur_power: Some(1),
            max_power: Some(1),
            ..Self::default()
        }
    }

    /// **The live-descriptor snapshot** — `0x5f0880`, the record's third writer beside the wire and
    /// the slot writer (VERIFIED wow-re `object-layer/scratch/party-record-live-snapshot.md` §1).
    /// The reference runs it at the instant a party/raid member's object leaves the object manager
    /// (`SMSG_DESTROY_OBJECT` and the `OUT_OF_RANGE` block take the same virtual), immediately
    /// before asking the server for the member's stats — which is why an out-of-range party frame
    /// never reads `0/0` at the despawn edge.
    ///
    /// Fields, in the binary's own order: `status` (bit0 online — an object you can see belongs to
    /// an online player, `0x5f088e c6 46 08 01`; bit3 ghost; bit1 PVP; bit4 FFA-PVP; bit2 dead),
    /// `+0x0a` cur HP, `+0x0c` max HP, `+0x09` power type, `+0x0e`/`+0x10` the power pair,
    /// `+0x12` level. **Raw** descriptor values, not the `Unit*` getters' display ones: the
    /// reference copies the field words (`5f08ea 66 8b 51 40`), and the raw→display divide happens
    /// at the *read*, in [`Self::shown_power`] — the same place the live path does it.
    ///
    /// **Three stated deviations** (decision 1640), each a field with no reader in benilla today:
    /// - `zone` and `position` are **left as they were** rather than overwritten. The reference
    ///   writes the *viewer's* zone (`[0xb4e314]`) and the object's world position; ours keeps the
    ///   last wire-reported pair, which the `_FULL` answer to the request corrects within a round
    ///   trip either way. Nothing draws party dots on the minimap yet — that is the consumer whose
    ///   arrival makes the position worth plumbing.
    /// - the 48-slot **aura** block and the **pet** block are not snapshotted: benilla feeds no
    ///   party-token aura list and resolves no `partypetN` token, so both would be write-only.
    /// - AFK/DND (`0x40`/`0x80`) are dropped from `status`, exactly as the reference's byte is
    ///   rewritten whole — those two bits live on the roster entry, which is what the party frame
    ///   actually overlays.
    pub fn snapshot_descriptor(&mut self, fields: &crate::messages::update_object::ObjectFields) {
        let mut status = member_status::ONLINE;
        if fields.player_is_ghost() {
            status |= member_status::GHOST;
        }
        // `UNIT_FIELD_FLAGS` PvP bit — the same word/bit the tooltip's PvP line reads.
        if fields.unit_flags() & 0x1000 != 0 {
            status |= member_status::PVP;
        }
        // `PLAYER_FLAGS` bit 7 (`0x5f08be c1 ea 07`).
        if fields.player_flags() & 0x80 != 0 {
            status |= member_status::PVP_FFA;
        }
        // `[desc+0x40] <= 0` — the raw health, not `unit_reads_dead`: the snapshot has no
        // dyn-flag leg, so a feigning member's record says alive (`5f08d3 85 c9; 7f 04`).
        if fields.unit_health().unwrap_or(0) == 0 {
            status |= member_status::DEAD;
        }
        self.status = Some(status);
        // The record's fields are `u16` (`+0x0a`..`+0x12`), and so is the wire's — the reference
        // takes the low word of each dword (`66 8b 51 40`), which is a truncation no 1.12
        // character can reach.
        let power_type = fields.unit_power_type();
        self.cur_hp = Some(fields.unit_health().unwrap_or(0) as u16);
        self.max_hp = Some(fields.unit_max_health().unwrap_or(0) as u16);
        self.power_type = Some(power_type);
        self.cur_power = Some(fields.unit_power(power_type).unwrap_or(0) as u16);
        self.max_power = Some(fields.unit_max_power(power_type).unwrap_or(0) as u16);
        self.level = Some(fields.unit_level().unwrap_or(0) as u16);
    }

    /// `UnitPowerType` on the record path — `+0x09`, `0` when the record has never carried one
    /// (the binding's own miss value).
    pub fn shown_power_type(&self) -> u8 {
        self.power_type.unwrap_or(0)
    }

    /// `UnitMana` on the record path (`0x517744`-`0x51775e`) — the stored power divided by
    /// [`power_display_scale`], exactly as the live-object leg divides. Without it an out-of-range
    /// warrior's rage bar reads ten times an in-range one's. Miss ⇒ `0`, the binding's own.
    pub fn shown_power(&self) -> u32 {
        u32::from(self.cur_power.unwrap_or(0)) / power_display_scale(self.shown_power_type())
    }

    /// `UnitManaMax` on the record path (`0x5178af`), the same divide.
    pub fn shown_max_power(&self) -> u32 {
        u32::from(self.max_power.unwrap_or(0)) / power_display_scale(self.shown_power_type())
    }
}

/// Read the aura tail shared by [`party_member_mask::AURAS`]/`PET_AURAS` (a `u32` bit mask) and
/// `AURAS_NEGATIVE`/`PET_AURAS_NEGATIVE` (a `u16` bit mask, passed widened): one `u16` spell id per
/// set bit, ascending bit order (VERIFIED `GroupHandler.cpp:624-630` positive, `634-644`
/// negative/pet-negative shape).
fn read_aura_spells(r: &mut impl Read, mask: u32, bits: u32) -> io::Result<Vec<u16>> {
    let mut spells = Vec::new();
    for bit in 0..bits {
        if mask & (1 << bit) != 0 {
            spells.push(read_u16_le(r)?);
        }
    }
    Ok(spells)
}

/// Read one `SMSG_PARTY_MEMBER_STATS`/`_FULL` body → `(guid, info)` (see [`PartyMemberStatsInfo`]).
/// The guid is PACKED (`GroupHandler.cpp:768`, `packet.guid.WriteAsPacked()` / `773`
/// `player->GetPackGUID()` — both build-5875 branches, `SUPPORTED_CLIENT_BUILD > 1_8_4`).
pub(super) fn read_party_member_stats(
    r: &mut impl Read,
) -> io::Result<(u64, PartyMemberStatsInfo)> {
    let guid = read_packed_guid(r)?;
    let mask = read_u32_le(r)?;
    let mut info = PartyMemberStatsInfo::default();

    if mask & party_member_mask::STATUS != 0 {
        info.status = Some(read_u8(r)?);
    }
    if mask & party_member_mask::CUR_HP != 0 {
        info.cur_hp = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::MAX_HP != 0 {
        info.max_hp = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::POWER_TYPE != 0 {
        info.power_type = Some(read_u8(r)?);
    }
    if mask & party_member_mask::CUR_POWER != 0 {
        info.cur_power = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::MAX_POWER != 0 {
        info.max_power = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::LEVEL != 0 {
        info.level = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::ZONE != 0 {
        info.zone = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::POSITION != 0 {
        let x = read_u16_le(r)? as i16;
        let y = read_u16_le(r)? as i16;
        info.position = Some((x, y));
    }
    if mask & party_member_mask::AURAS != 0 {
        let pos_mask = read_u32_le(r)?;
        info.auras = Some(read_aura_spells(r, pos_mask, 32)?);
    }
    if mask & party_member_mask::AURAS_NEGATIVE != 0 {
        let neg_mask = u32::from(read_u16_le(r)?);
        info.auras_negative = Some(read_aura_spells(r, neg_mask, 16)?);
    }
    if mask & party_member_mask::PET_GUID != 0 {
        info.pet_guid = Some(read_u64_le(r)?);
    }
    if mask & party_member_mask::PET_NAME != 0 {
        info.pet_name = Some(read_cstring(r)?);
    }
    if mask & party_member_mask::PET_MODEL_ID != 0 {
        info.pet_model_id = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::PET_CUR_HP != 0 {
        info.pet_cur_hp = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::PET_MAX_HP != 0 {
        info.pet_max_hp = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::PET_POWER_TYPE != 0 {
        info.pet_power_type = Some(read_u8(r)?);
    }
    if mask & party_member_mask::PET_CUR_POWER != 0 {
        info.pet_cur_power = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::PET_MAX_POWER != 0 {
        info.pet_max_power = Some(read_u16_le(r)?);
    }
    if mask & party_member_mask::PET_AURAS != 0 {
        let pos_mask = read_u32_le(r)?;
        info.pet_auras = Some(read_aura_spells(r, pos_mask, 32)?);
    }
    if mask & party_member_mask::PET_AURAS_NEGATIVE != 0 {
        let neg_mask = u32::from(read_u16_le(r)?);
        info.pet_auras_negative = Some(read_aura_spells(r, neg_mask, 16)?);
    }
    Ok((guid, info))
}

/// Read `MSG_MINIMAP_PING`, the inbound (rebroadcast) shape (VERIFIED vmangos
/// `Handlers/GroupHandler.cpp:382-391`, `HandleMinimapPingOpcode`): full 8-byte `u64 guid` (the
/// server stamps the pinger's own guid on), `f32 x`, `f32 y`.
pub(super) fn read_minimap_ping(r: &mut impl Read) -> io::Result<(u64, f32, f32)> {
    Ok((read_u64_le(r)?, read_f32_le(r)?, read_f32_le(r)?))
}

/// One decoded `MSG_RAID_TARGET_UPDATE` server body (VERIFIED vmangos `Server/Packets/
/// Group.cpp:132-147`, `RaidTargetUpdateDelta`/`RaidTargetUpdateAll::AppendBodyTo`): a leading `u8`
/// mode byte picks the shape — `0` one changed icon, `1` the whole current icon set (only
/// currently-set icons ride the list; none marked sends an empty list, not a fixed 8-slot array).
/// Both shapes carry FULL 8-byte guids, not packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaidTargetUpdate {
    Delta { icon: u8, guid: u64 },
    List(Vec<(u8, u64)>),
}

/// Read one `MSG_RAID_TARGET_UPDATE` server body (see [`RaidTargetUpdate`]).
pub(super) fn read_raid_target_update(r: &mut &[u8]) -> io::Result<RaidTargetUpdate> {
    let mode = read_u8(r)?;
    if mode == 0 {
        Ok(RaidTargetUpdate::Delta {
            icon: read_u8(r)?,
            guid: read_u64_le(r)?,
        })
    } else {
        let mut entries = Vec::new();
        while !r.is_empty() {
            entries.push((read_u8(r)?, read_u64_le(r)?));
        }
        Ok(RaidTargetUpdate::List(entries))
    }
}

/// One decoded `MSG_RAID_READY_CHECK` server body (VERIFIED vmangos `Server/Packets/
/// Group.cpp:94-96` the empty request, `:126-130` the answer): empty = the leader just started a
/// check; non-empty = one member's answer, forwarded to the leader only (`ObjectGuid senderGuid`
/// FULL, not packed, + `u8 state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyCheck {
    Started,
    Answer { guid: u64, ready: u8 },
}

/// Read one `MSG_RAID_READY_CHECK` server body (see [`ReadyCheck`]).
pub(super) fn read_ready_check(r: &mut &[u8]) -> io::Result<ReadyCheck> {
    if r.is_empty() {
        return Ok(ReadyCheck::Started);
    }
    Ok(ReadyCheck::Answer {
        guid: read_u64_le(r)?,
        ready: read_u8(r)?,
    })
}

/// Body of `CMSG_GROUP_INVITE` (VERIFIED vmangos `Group.cpp:4-7`,
/// `GroupInvite::ReadFromWorldPacket`): one cstring, the invited player's name.
pub fn group_invite(member_name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(member_name.len() + 1);
    body.extend_from_slice(member_name.as_bytes());
    body.push(0);
    body
}

/// Body of `CMSG_GROUP_ACCEPT` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp` —
/// `NullClientPacket`): empty.
pub fn group_accept() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GROUP_DECLINE` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp` —
/// `NullClientPacket`): empty. The inviter sees `SMSG_GROUP_DECLINE`.
pub fn group_decline() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GROUP_UNINVITE` (VERIFIED vmangos `Group.cpp:9-12`,
/// `GroupUninvite::ReadFromWorldPacket`): one cstring, the kicked player's name.
pub fn group_uninvite(member_name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(member_name.len() + 1);
    body.extend_from_slice(member_name.as_bytes());
    body.push(0);
    body
}

/// Body of `CMSG_GROUP_UNINVITE_GUID` (VERIFIED vmangos `Group.cpp:14-17`,
/// `GroupUninviteGuid::ReadFromWorldPacket`): one full 8-byte guid — the raid-frame right-click kick.
pub fn group_uninvite_guid(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_GROUP_SET_LEADER` (VERIFIED vmangos `Group.h:102-113` + `Group.cpp:57-64`,
/// `GroupSetLeader::ReadFromWorldPacket`, the `SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_11_2` branch
/// active for 5875): one full 8-byte guid — the 1.12 wire dropped the older name-based form.
pub fn group_set_leader(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_LOOT_METHOD` (VERIFIED vmangos `Group.h:51-60` + `Group.cpp:26-31`,
/// `LootMethod::ReadFromWorldPacket`): `u32 method` (`0` free-for-all, `1` round-robin, `2`
/// master, `3` group, `4` need-before-greed), full 8-byte `u64 loot_master` guid (ignored unless
/// `method == 2`), `u32 threshold` (an `ItemQualities` value, 2..4 in practice).
pub fn loot_method(method: u32, loot_master: u64, threshold: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&method.to_le_bytes());
    body.extend_from_slice(&loot_master.to_le_bytes());
    body.extend_from_slice(&threshold.to_le_bytes());
    body
}

/// Body of `CMSG_GROUP_DISBAND` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp` —
/// `NullClientPacket`): empty.
pub fn group_disband() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GROUP_RAID_CONVERT` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp` —
/// `NullClientPacket`): empty — converts the sender's party into a raid.
pub fn group_raid_convert() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_REQUEST_PARTY_MEMBER_STATS` (VERIFIED vmangos `Group.h:41-49` + `Group.cpp:20-23`,
/// `RequestPartyMemberStats::ReadFromWorldPacket`): one full 8-byte guid — the target whose stats
/// we're asking for. Answered by `SMSG_PARTY_MEMBER_STATS_FULL` (or its offline-miss shape).
pub fn request_party_member_stats(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_GROUP_CHANGE_SUB_GROUP` (VERIFIED vmangos `Group.cpp:45-49`,
/// `GroupChangeSubGroup::ReadFromWorldPacket`): cstring member name + `u8` destination subgroup
/// index — raid-only (a party has just one subgroup).
pub fn group_change_sub_group(name: &str, group_nr: u8) -> Vec<u8> {
    let mut body = Vec::with_capacity(name.len() + 2);
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body.push(group_nr);
    body
}

/// Body of `CMSG_GROUP_SWAP_SUB_GROUP` (VERIFIED vmangos `Group.cpp:51-55`,
/// `GroupSwapSubGroup::ReadFromWorldPacket`): two cstrings — the member to move, then the member
/// whose subgroup slot it swaps into.
pub fn group_swap_sub_group(name: &str, swap_with: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(name.len() + swap_with.len() + 2);
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body.extend_from_slice(swap_with.as_bytes());
    body.push(0);
    body
}

/// Body of `CMSG_GROUP_ASSISTANT_LEADER` (VERIFIED vmangos `Group.h:115-127` + `Group.cpp:66-74`,
/// `GroupAssistantLeader::ReadFromWorldPacket`, the 1.12 guid branch): full 8-byte guid + `u8 flag`
/// (`1` grant raid-assistant, `0` revoke).
pub fn group_assistant_leader(guid: u64, grant: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(9);
    body.extend_from_slice(&guid.to_le_bytes());
    body.push(u8::from(grant));
    body
}

/// Body of `MSG_MINIMAP_PING`, our own ping (VERIFIED vmangos `Group.cpp:33-37`,
/// `MinimapPing::ReadFromWorldPacket`): `f32 x, f32 y` — no guid (the server stamps ours on before
/// relaying, `Handlers/GroupHandler.cpp:382-391`).
pub fn minimap_ping(x: f32, y: f32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&x.to_le_bytes());
    body.extend_from_slice(&y.to_le_bytes());
    body
}

/// Body of `MSG_RAID_TARGET_UPDATE`, setting/clearing one icon (VERIFIED vmangos
/// `Group.cpp:77-82`, `RaidTargetUpdate::ReadFromWorldPacket`): `u8 icon` (0..7) + full 8-byte
/// `u64 guid` — `guid == 0` clears that icon. The client never sends the server's mode byte
/// (compare [`RaidTargetUpdate`]); [`raid_target_request`] disambiguates purely by icon value.
pub fn raid_target_set(icon: u8, guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(9);
    body.push(icon);
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// Body of `MSG_RAID_TARGET_UPDATE`, asking the current icon set (VERIFIED vmangos
/// `Group.cpp:80-81`: `iconId != 0xFF` gates the trailing guid read): the single byte `0xFF` — no
/// guid follows. Answered by the server's mode-1 full list ([`RaidTargetUpdate::List`]).
pub fn raid_target_request() -> Vec<u8> {
    vec![0xFF]
}

/// Body of `MSG_RAID_READY_CHECK`, starting a check (VERIFIED vmangos
/// `RaidReadyCheckFromClient::ReadFromWorldPacket`, `Group.cpp:84-92`, the `state` unset case):
/// empty — the leader-only trigger; the server broadcasts the request to the rest of the raid.
pub fn ready_check_start() -> Vec<u8> {
    Vec::new()
}

/// Body of `MSG_RAID_READY_CHECK`, answering one (VERIFIED vmangos `Group.cpp:84-92`, the `state`
/// set case): one `u8`, `1` ready / `0` not — no guid; the server already knows the sender from
/// the session and forwards it to the raid leader alone.
pub fn ready_check_answer(ready: bool) -> Vec<u8> {
    vec![u8::from(ready)]
}

/// One row of `SMSG_RAID_INSTANCE_INFO` — a raid lockout the character is bound to (VERIFIED
/// vmangos `Objects/Player.cpp::Player::SendRaidInfo`: the writer walks `m_boundInstances` and
/// emits a row for every **permanent** bind, so a heroic-style temporary bind never appears).
///
/// `reset` is a REMAINING duration in seconds (`resetTime - time(nullptr)` on the server), not an
/// absolute timestamp — the Lua binding hands it straight to `SecondsToTime`, and reading it as a
/// timestamp would print "Resets in 57 years".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaidInstanceEntry {
    /// `Map.dbc` id — the *name* the UI shows is the client's own DBC lookup, never on the wire.
    pub map: u32,
    /// Seconds until the lockout resets.
    pub reset: u32,
    /// The instance id, which is what the UI shows beside the name (`GetSavedInstanceInfo`'s
    /// second return).
    pub instance: u32,
}

/// Read an `SMSG_RAID_INSTANCE_INFO` body (see [`RaidInstanceEntry`]): `u32 count` then that many
/// 12-byte rows. The count is authoritative — the server writes it back over its placeholder — so
/// a short body is a malformed packet rather than a silent truncation.
pub(super) fn read_raid_instance_info(r: &mut &[u8]) -> io::Result<Vec<RaidInstanceEntry>> {
    let count = read_u32_le(r)?;
    // A cap before the allocation: `count` is attacker-controlled in the general case, and the
    // real client's own list is `MAX_RAID_INFOS`-bounded at the UI. 1024 is far above anything a
    // server can legitimately send and far below a memory problem.
    let mut out = Vec::with_capacity((count as usize).min(1024));
    for _ in 0..count {
        out.push(RaidInstanceEntry {
            map: read_u32_le(r)?,
            reset: read_u32_le(r)?,
            instance: read_u32_le(r)?,
        });
    }
    Ok(out)
}

/// Body of `CMSG_REQUEST_RAID_INFO` (VERIFIED vmangos `Handlers/GroupHandler.cpp`'s
/// `HandleRequestRaidInfoOpcode` — the handler reads nothing and answers `SendRaidInfo()`): empty.
pub fn request_raid_info() -> Vec<u8> {
    Vec::new()
}
