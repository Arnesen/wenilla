//! The **cast pipeline** — a cast's whole life on the wire: the four outbound `CMSG_CAST_SPELL`
//! shapes, the server's verdict, the start/launch pair every observer sees, the interrupt and
//! pushback notices, the channel timer, and the aura a cast leaves behind. Every layout here is
//! VERIFIED against vmangos source (cited per item); the opcodes are in [`super::opcode`] (verified
//! `Opcodes_1_12_1.h`).
//!
//! This file was once the whole spell/combat/bar/progression wire in one 1061-line block; decision
//! 0640 peeled off [`super::spellbook`], [`super::action_bar`], [`super::attack`],
//! [`super::combat_log`], [`super::progression`] and [`super::pose`], leaving the cast itself.
//! Mirrored by `world::writer::spells`.
//!
//! Two things about the shape that are easy to get wrong and are load-bearing here:
//!
//! - [`SpellCastTargets`]'s decode must follow vmangos' **write**-side branch order bit for bit, not
//!   the symmetric-looking read side — the writer's if/else-if chain emits exactly one packed guid
//!   when several target bits are set, and guessing differently desyncs the stream.
//! - Nothing about missile travel rides `SMSG_SPELL_GO`. It is sent at **launch**; the server
//!   schedules impact itself off `Spell.dbc` Speed (decision 0099).
//!
//! The aura pair lives here rather than in a family of its own because an aura is what a spell
//! leaves behind, and the wire says so: `CMSG_CANCEL_AURA` is addressed **by spell id, not by aura
//! slot**. Aura *state* is descriptor data (`UNIT_FIELD_AURA`), not a packet.

use std::io::{self, Read};

use crate::wire::{
    read_cstring, read_packed_guid, read_u16_le, read_u32_le, read_u64_le, read_u8, Vector3d,
};

/// `SMSG_CAST_RESULT`'s verdict: `u32 spellId, u8 status` — status `0` (`SPELL_RESULT_STATUS_OKAY`)
/// ends the packet, status `2` (`SPELL_RESULT_STATUS_FAIL`) appends a `u8` failure reason and then
/// up to two **reason-specific argument words** (vmangos `CastResult::AppendBodyTo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOutcome {
    Ok,
    Failed {
        reason: u8,
        /// The failure's first argument word, when the server sent one — the `%s` source of the
        /// client's argument-formatted messages (the arm table at `0x6e1d8e`): a
        /// `SpellFocusObject.dbc` id for `REQUIRES_SPELL_FOCUS` (0x5e), an `AreaTable.dbc` id for
        /// `REQUIRES_AREA` (0x5d), an `EquippedItemClass` for the 0x19–0x1b family, a
        /// permanent-cooldown flag for `NOT_READY` (0x3c). `None` = the packet ended at the reason.
        arg: Option<u32>,
    },
}

/// Read `SMSG_CAST_RESULT` → `(spell_id, outcome)`.
///
/// The trailing argument words are **positional, gated on the remaining body length** — and that
/// is the reference's own decode, not merely a convenient one. `CastResultHandler 0x6e7330`
/// (registered at `0x6e7155`) reads `u32 spellId`, `u8 status`, then on `status == 2` a
/// `u8 reason`, then each word only `if cursor < size` (`0x6e737a` and `0x6e738c`, comparing
/// CDataStore `+0x14` against `+0x10`) — never keyed off the reason value (wow-re
/// `system/spell/scratch/cast-fail-strings.md` §WIRE-ARGS, §5 cross-checked). It has to be: vmangos
/// writes `if (arg1 || arg2) << u32 arg1; if (arg2) << u32 arg2;`
/// (`Server/Packets/Spell.cpp`, `CastResult::AppendBodyTo`), so a reason whose argument is
/// legitimately 0 with no second argument behind it sends no word at all, and a reason-keyed decode
/// would desync on exactly that case.
///
/// **The reference's absent value is `-1`, not zero** (`0x6e736e or eax,0xffffffff` initializes both
/// slots before the gated reads). `None` here is that sentinel: every consumer treats it and an
/// out-of-range id the same way — decline the fill — so the two encodings are behaviourally
/// identical, and `Option` says which case it is at the type level.
///
/// The second word is read to keep the shape documented, then dropped: only the
/// `EQUIPPED_ITEM_CLASS*` family (0x19–0x1b) sends one, and that arm's `%s` fill reads the failing
/// spell's own `EquippedItemClass`/`EquippedItemSubClassMask` columns — the same two values the
/// server copied out of `SpellEntry` to build the packet (`Spell::SendCastResult`).
pub(super) fn read_cast_result(r: &mut &[u8]) -> io::Result<(u32, CastOutcome)> {
    let spell_id = read_u32_le(r)?;
    let status = read_u8(r)?;
    let outcome = if status == 2 {
        let reason = read_u8(r)?;
        let arg = (r.len() >= 4).then(|| read_u32_le(r)).transpose()?;
        let _arg2 = (r.len() >= 4).then(|| read_u32_le(r)).transpose()?;
        CastOutcome::Failed { reason, arg }
    } else {
        CastOutcome::Ok
    };
    Ok((spell_id, outcome))
}

// --- the spell-visual pipeline wire (decision 0099 phase 1) -----------------------------------------
//
// `SpellCastTargets` bit flags (vmangos `SpellDefines.h:96-113`, `SpellCastTargetFlags`) needed to
// decode the `write()` shape (`SpellCastTargetsInfo.cpp:180-234`) SMSG_SPELL_START/GO carry.
const TARGET_FLAG_UNIT: u16 = 0x0002;
const TARGET_FLAG_ITEM: u16 = 0x0010;
const TARGET_FLAG_SOURCE_LOCATION: u16 = 0x0020;
const TARGET_FLAG_DEST_LOCATION: u16 = 0x0040;
const TARGET_FLAG_CORPSE_ENEMY: u16 = 0x0200;
const TARGET_FLAG_GAMEOBJECT: u16 = 0x0800;
const TARGET_FLAG_TRADE_ITEM: u16 = 0x1000;
const TARGET_FLAG_STRING: u16 = 0x2000;
const TARGET_FLAG_CORPSE_ALLY: u16 = 0x8000;

/// A decoded `SpellCastTargets` (vmangos `SpellCastTargetsInfo.cpp:180-234`, the **write** side —
/// asymmetric from the client→server `read()`: when more than one of UNIT/GAMEOBJECT/CORPSE_* is set,
/// the writer's if/else-if chain emits exactly **one** packed guid, by priority UNIT > GAMEOBJECT >
/// CORPSE_ALLY|CORPSE_ENEMY — never happens for a real spell, but the decode must follow the same
/// branch order bit for bit or it desyncs the stream). Surfaces what a consumer needs now
/// (`unit_target`, `go_target`, `dest`); item/corpse/source/string targets are read to keep the cursor
/// aligned and dropped — no consumer for them yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpellCastTargets {
    pub mask: u16,
    pub unit_target: Option<u64>,
    /// The GameObject this cast targets (`TARGET_FLAG_GAMEOBJECT`) — an open-lock cast on a chest / locked
    /// door rides here. Surfaced for the GO lid/door open animation (decision 0250); a unit spell leaves
    /// it `None`. Mutually exclusive with `unit_target` in practice (the writer emits one guid).
    pub go_target: Option<u64>,
    pub dest: Option<Vector3d>,
}

fn read_spell_cast_targets(r: &mut impl Read) -> io::Result<SpellCastTargets> {
    let mask = read_u16_le(r)?;
    let mut unit_target = None;
    let mut go_target = None;
    if mask
        & (TARGET_FLAG_UNIT
            | TARGET_FLAG_GAMEOBJECT
            | TARGET_FLAG_CORPSE_ENEMY
            | TARGET_FLAG_CORPSE_ALLY)
        != 0
    {
        // Exactly one packed guid rides here (see the struct doc); the writer's priority is UNIT >
        // GAMEOBJECT, so mirror that branch order (UNIT wins if both bits are set, never in practice).
        let guid = read_packed_guid(r)?;
        if mask & TARGET_FLAG_UNIT != 0 {
            unit_target = Some(guid);
        } else if mask & TARGET_FLAG_GAMEOBJECT != 0 {
            go_target = Some(guid);
        }
    }
    if mask & (TARGET_FLAG_ITEM | TARGET_FLAG_TRADE_ITEM) != 0 {
        let _item_guid = read_packed_guid(r)?;
    }
    if mask & TARGET_FLAG_SOURCE_LOCATION != 0 {
        let _src = Vector3d::read(r)?;
    }
    let dest = if mask & TARGET_FLAG_DEST_LOCATION != 0 {
        Some(Vector3d::read(r)?)
    } else {
        None
    };
    if mask & TARGET_FLAG_STRING != 0 {
        let _string_target = read_cstring(r)?;
    }
    Ok(SpellCastTargets {
        mask,
        unit_target,
        go_target,
        dest,
    })
}

/// `CAST_FLAG_AMMO` (vmangos `Spell.h:53`) — the projectile-visual bit, set for every ranged spell on
/// both `SMSG_SPELL_START` and `SMSG_SPELL_GO`; its presence gates the trailing ammo block.
const CAST_FLAG_AMMO: u16 = 0x0020;

/// Read the ammo block (`Spell::WriteAmmoToPacket`, `Spell.cpp:4540-4606`): `u32 displayId, u32
/// inventoryType`. Only `displayId` has a consumer today (the projectile model, phase 4/5) —
/// `inventoryType` is read to stay aligned and dropped.
fn read_ammo(r: &mut impl Read) -> io::Result<u32> {
    let display_id = read_u32_le(r)?;
    let _inventory_type = read_u32_le(r)?;
    Ok(display_id)
}

/// One decoded `SMSG_SPELL_START` — a non-triggered cast began, instants included (`cast_time_ms ==
/// 0` — the precast trigger, decision 0099 phase 1). VERIFIED vmangos `Spell::SendSpellStart`
/// (`Spell.cpp:4468-4503`): `item_or_caster` pguid (the cast item's guid when one is in play, else
/// the caster's own — `WriteGuidHelper`, `Spell.cpp:4453-4466`) · `caster` pguid (`m_casterUnit` —
/// **empty, i.e. guid 0, whenever the caster is a GameObject**, which never sets that member;
/// [`crate::events`]' `spell_caster` is what resolves the pair) · `u32 spellId` · `u16 castFlags` (always `CAST_FLAG_UNKNOWN2` 0x2, +
/// `CAST_FLAG_AMMO` 0x20 for a ranged spell) · `u32` remaining cast-time ms (`m_timer`) ·
/// [`SpellCastTargets`] · the ammo block iff `castFlags & 0x20`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpellStart {
    pub item_or_caster: u64,
    pub caster: u64,
    pub spell_id: u32,
    pub cast_flags: u16,
    pub cast_time_ms: u32,
    pub targets: SpellCastTargets,
    pub ammo_display_id: Option<u32>,
}

pub(super) fn read_spell_start(r: &mut impl Read) -> io::Result<SpellStart> {
    let item_or_caster = read_packed_guid(r)?;
    let caster = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let cast_flags = read_u16_le(r)?;
    let cast_time_ms = read_u32_le(r)?;
    let targets = read_spell_cast_targets(r)?;
    let ammo_display_id = if cast_flags & CAST_FLAG_AMMO != 0 {
        Some(read_ammo(r)?)
    } else {
        None
    };
    Ok(SpellStart {
        item_or_caster,
        caster,
        spell_id,
        cast_flags,
        cast_time_ms,
        targets,
        ammo_display_id,
    })
}

/// `SPELL_MISS_REFLECT` (vmangos `SpellDefines.h:173`) — the one `SpellMissInfo` that carries a
/// trailing byte (the reflected spell's own outcome against its new target).
const SPELL_MISS_REFLECT: u8 = 11;

/// One decoded `SMSG_SPELL_GO` — the cast launched. VERIFIED vmangos `Spell::SendSpellGo`
/// (`Spell.cpp:4505-4538`) + its target-list writer `WriteSpellGoTargets` (`Spell.cpp:4608-4659`):
/// the same guid pair + spellId as [`SpellStart`] (caster slot 2 empty for a GameObject caster —
/// see there) · `u16 castFlags` (always `CAST_FLAG_UNKNOWN9`
/// 0x100, + `CAST_FLAG_AMMO` 0x20 for a ranged spell) · `u8` hit count + that many **raw** (unpacked)
/// `u64` hit guids · `u8` miss count + that many `{u64 guid, u8 SpellMissInfo, u8 reflectResult iff
/// the reason is `SPELL_MISS_REFLECT`}` · [`SpellCastTargets`] · the ammo block iff `castFlags &
/// 0x20`. Sent at **launch** — the server schedules impact itself off `Spell.dbc` Speed, so nothing
/// about missile travel rides this packet (decision 0099).
#[derive(Debug, Clone, PartialEq)]
pub struct SpellGo {
    pub item_or_caster: u64,
    pub caster: u64,
    pub spell_id: u32,
    pub cast_flags: u16,
    pub hits: Vec<u64>,
    /// `(guid, SpellMissInfo reason)` — the reflect-outcome byte (present only when `reason ==
    /// SPELL_MISS_REFLECT`) is read to stay aligned and dropped; no consumer yet.
    pub misses: Vec<(u64, u8)>,
    pub targets: SpellCastTargets,
    pub ammo_display_id: Option<u32>,
}

pub(super) fn read_spell_go(r: &mut impl Read) -> io::Result<SpellGo> {
    let item_or_caster = read_packed_guid(r)?;
    let caster = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let cast_flags = read_u16_le(r)?;

    let hit_count = read_u8(r)?;
    let mut hits = Vec::with_capacity(hit_count as usize);
    for _ in 0..hit_count {
        hits.push(read_u64_le(r)?); // raw guid (Spell.cpp:4627,4635) — the hit list is never packed
    }
    let miss_count = read_u8(r)?;
    let mut misses = Vec::with_capacity(miss_count as usize);
    for _ in 0..miss_count {
        let guid = read_u64_le(r)?;
        let reason = read_u8(r)?;
        if reason == SPELL_MISS_REFLECT {
            let _reflect_result = read_u8(r)?;
        }
        misses.push((guid, reason));
    }

    let targets = read_spell_cast_targets(r)?;
    let ammo_display_id = if cast_flags & CAST_FLAG_AMMO != 0 {
        Some(read_ammo(r)?)
    } else {
        None
    };
    Ok(SpellGo {
        item_or_caster,
        caster,
        spell_id,
        cast_flags,
        hits,
        misses,
        targets,
        ammo_display_id,
    })
}

/// Read `SMSG_SPELL_FAILED_OTHER` → `(caster, spell_id)` (vmangos `Spell::SendInterrupted`,
/// `Spell.cpp:4780-4789`): the broadcast cast-cancel notice for **observers** — a raw (unpacked)
/// `u64` guid + `u32` spellId. Our own cast's failure rides `SMSG_CAST_RESULT` instead;
/// `SMSG_SPELL_FAILURE` is never constructed server-side (decision 0099).
pub(super) fn read_spell_failed_other(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let caster = read_u64_le(r)?;
    let spell_id = read_u32_le(r)?;
    Ok((caster, spell_id))
}

/// Read `SMSG_SPELL_DELAYED` → `(caster, delay_ms)` (vmangos `Spell::Delayed`, `Spell.cpp:7472`):
/// a raw (unpacked) `u64` caster guid + `u32` pushback time in ms, sent **to the caster** when a
/// pushback-eligible cast takes damage (Fireball carries `DAMAGE_PUSHBACK`; the server extends its
/// own cast timer by `delay_ms`). The cast bar shifts its window out by the same (decision 0256).
pub(super) fn read_spell_delayed(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let caster = read_u64_le(r)?;
    let delay_ms = read_u32_le(r)?;
    Ok((caster, delay_ms))
}

/// Read `MSG_CHANNEL_START` → `(spell_id, duration_ms)` (vmangos `Spell::SendChannelStart`,
/// `Spell.cpp:4951-4954`): `u32 spellId` + `u32 duration` — **self-only** (SendDirectMessage to
/// the casting player; no guid on the wire). The cast bar's channel-open edge (decision 0137).
pub(super) fn read_channel_start(r: &mut impl Read) -> io::Result<(u32, u32)> {
    let spell_id = read_u32_le(r)?;
    let duration_ms = read_u32_le(r)?;
    Ok((spell_id, duration_ms))
}

/// Read `MSG_CHANNEL_UPDATE` → `remaining_ms` (vmangos `Player::SendChannelUpdate`,
/// `Player.cpp:21106-21110`): a single `u32` time-left — **self-only**, `0` = the channel is over
/// (sent on natural end and on interrupt alike). The cast bar's channel tick/close (decision 0137).
pub(super) fn read_channel_update(r: &mut impl Read) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `SMSG_UPDATE_AURA_DURATION` → `(slot, remaining_ms)` (vmangos
/// `SpellAuraHolder::UpdateAuraDuration`, `SpellAuras.cpp:7511-7523`): a `u8` `UNIT_FIELD_AURA` slot
/// index and a `u32` of milliseconds left. **Self-only** — it goes to the aura's target, never to
/// the caster or an onlooker — and never sent for a permanent aura, so an occupied slot that has no
/// duration is the reference's "until cancelled" (decision 0255).
pub(super) fn read_update_aura_duration(r: &mut impl Read) -> io::Result<(u8, u32)> {
    let slot = read_u8(r)?;
    let remaining_ms = read_u32_le(r)?;
    Ok((slot, remaining_ms))
}

/// Read `SMSG_PLAY_SPELL_VISUAL` → `(unit, kit_id)` (vmangos
/// `WorldPackets::Spell::PlaySpellVisual::AppendBodyTo`, `Server/Packets/Spell.cpp:54-58`): a raw
/// (unpacked) `u64` guid + `u32` kit id, bounds-checked against `SpellVisualKit.dbc` and played
/// at the client's hardcoded stage 0 (`0x6e98d0` — the eat/drink cadence; decision 0280).
pub(super) fn read_play_spell_visual(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let unit = read_u64_le(r)?;
    let kit_id = read_u32_le(r)?;
    Ok((unit, kit_id))
}

/// Body of `CMSG_CAST_SPELL` (vmangos `CastSpell::ReadFromWorldPacket` → `SpellCastTargets::read`):
/// `u32 spellId` + the target block. `None` = a self/implicit cast — mask `TARGET_FLAG_SELF (0)`,
/// nothing follows (the server fills the target from the spell's implicit targeting).
/// `Some(guid)` = an explicit unit target — mask `TARGET_FLAG_UNIT (0x0002)` + the guid **packed**
/// (`ReadAsPackedClientBuildAware` is the packed reader for builds > 1.8.4).
pub fn cast_spell(spell_id: u32, target: Option<u64>) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&spell_id.to_le_bytes());
    match target {
        None => body.extend_from_slice(&0u16.to_le_bytes()),
        Some(guid) => {
            body.extend_from_slice(&2u16.to_le_bytes());
            crate::wire::write_packed_guid(guid, &mut body).expect("vec write");
        }
    }
    body
}

/// Body of `CMSG_CAST_SPELL` aimed at a **GameObject** (decision 0239): the OPEN_LOCK cast a
/// right-click on a locked chest / mining vein / herb node sends instead of `CMSG_GAMEOBJ_USE`, and
/// the targeting cursor's world commit (decision 0939). `spell_id`, then the target mask
/// `GAMEOBJECT (0x0800)` **alone**, then the GameObject's **packed** guid — the one field that mask
/// reads. Distinct from [`cast_spell`]'s unit/self target shape (flag `0x2` + packed guid).
///
/// **`TARGET_FLAG_LOCKED (0x4000)` is not in this mask** (decision 0939, correcting 0239/0769). The
/// bit lives in the *targeting* word `0xcecac0`: the implicit-target switch ORs it there
/// (`6e44e1: orl $0x4000, %ebx` — `%ebx` is the word that switch is accumulating, not the outgoing
/// mask), and `BindTarget 0x6e5b40`'s GameObject arm then **consumes** it — `6e5f60 testb $0x48, %ch`
/// on the word, `6e5f69 orb $0x8, 0xceac5d` (⇒ `0x0800`) into the wire mask, `6e5f70` clearing the
/// word's own `0x4800`. A whole-image census of every write to the 16-bit mask at `0xceac5c` (one
/// `movw` restore plus eleven `orb` sites) contains no `0x4000` anywhere: the real client never puts
/// `LOCKED` on the wire, in this packet or any other.
pub fn cast_spell_gameobject(spell_id: u32, go_guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&TARGET_FLAG_GAMEOBJECT.to_le_bytes());
    crate::wire::write_packed_guid(go_guid, &mut body).expect("vec write");
    body
}

/// Body of `CMSG_CAST_SPELL` aimed at an **ITEM** (decision 0437 phase 3): the enchant/poison
/// cast the CraftFrame's item pick completes. `spell_id`, mask `TARGET_FLAG_ITEM (0x0010)`, then
/// the item's **packed** guid — the one field the mask reads (vmangos `SpellCastTargets::read`,
/// `SpellCastTargetsInfo.cpp:159-160`: `ITEM | TRADE_ITEM → one packed guid`; the trade-window
/// sentinel form (`TRADE_ITEM 0x1000`) is the player-trade arc's, not built here).
pub fn cast_spell_item(spell_id: u32, item_guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&TARGET_FLAG_ITEM.to_le_bytes());
    crate::wire::write_packed_guid(item_guid, &mut body).expect("vec write");
    body
}

/// Body of `CMSG_CAST_SPELL` aimed at a **ground point** (decision 0792): the targeting-cursor
/// commit for a `Targets & 0x40` spell (Blizzard, Flamestrike, Rain of Fire…). `spell_id`, mask
/// `DEST_LOCATION (0x0040)`, then the destination as three `f32` **WoW world coords** — the one
/// field the mask reads (vmangos `SpellCastTargets::read`, `SpellCastTargetsInfo.cpp:169-174`:
/// `DEST_LOCATION → x,y,z`, `IsValidMapCoord`-gated). The real client ships the same shape from
/// `SPELLCAST+0x3c..0x44` (`BindLocation 0x6e60f0` → `SendCast 0x6e54f0`, wow-re `wave-cast.md`).
pub fn cast_spell_at_dest(spell_id: u32, dest: [f32; 3]) -> Vec<u8> {
    let mut body = Vec::with_capacity(18);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&TARGET_FLAG_DEST_LOCATION.to_le_bytes());
    for c in dest {
        body.extend_from_slice(&c.to_le_bytes());
    }
    body
}

/// Body of `CMSG_CANCEL_AURA` (vmangos `WorldPackets::Spell::CancelAura`, `Server/Packets/Spell.h:55-62`):
/// one `u32` spell id. The server cancels **by spell, not by slot** — `HandleCancelAuraOpcode`
/// (`SpellHandler.cpp:333-405`) looks the spell up, refuses passives, `SPELL_ATTR_NO_AURA_CANCEL`
/// spells and debuffs, then calls `RemoveAurasDueToSpellByCancel`. The wire's own
/// `AURA_FLAG_CANCELABLE` nibble bit is the matching client-side gate (decision 0255).
pub fn cancel_aura(spell_id: u32) -> Vec<u8> {
    spell_id.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_spell_gameobject_body_golden() {
        // spell_id (LE) + target mask GAMEOBJECT = 0x0800 (LE `00 08`) + packed guid 0x1234
        // (mask 0x03, bytes 34 12). The `0x4000` LOCKED bit is deliberately absent: it is a
        // *targeting-word* bit that `BindTarget 0x6e5b40` consumes, never a wire bit (decision
        // 0939 — the whole-image census of writes to `0xceac5c` has no `0x4000`).
        assert_eq!(
            cast_spell_gameobject(1, 0x1234),
            [0x01, 0x00, 0x00, 0x00, 0x00, 0x08, 0x03, 0x34, 0x12],
            "CMSG_CAST_SPELL (GameObject/OPEN_LOCK) body"
        );
        // The unit-target twin still carries flag 0x2, not 0x800 — the two shapes stay distinct.
        assert_eq!(cast_spell(1, None), [0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn cast_spell_at_dest_body_golden() {
        // spell_id 10 (Blizzard, LE) + mask DEST_LOCATION 0x0040 (LE `40 00`) + the dest Vec3 as
        // three f32 LE: 1.0 = 00 00 80 3F, -2.5 = 00 00 20 C0, 3.0 = 00 00 40 40. VERIFIED against
        // vmangos `SpellCastTargets::read` (mask → DEST branch reads exactly x, y, z).
        assert_eq!(
            cast_spell_at_dest(10, [1.0, -2.5, 3.0]),
            [
                0x0A, 0x00, 0x00, 0x00, // spell id
                0x40, 0x00, // TARGET_FLAG_DEST_LOCATION
                0x00, 0x00, 0x80, 0x3F, // x = 1.0
                0x00, 0x00, 0x20, 0xC0, // y = -2.5
                0x00, 0x00, 0x40, 0x40, // z = 3.0
            ],
            "CMSG_CAST_SPELL (ground dest) body"
        );
    }

    #[test]
    fn aura_bodies_golden() {
        // CMSG_CANCEL_AURA: a lone u32 spell id, LE. 1126 (Mark of the Wild) = 0x0000_0466. The
        // server cancels by spell — there is no slot byte to get wrong.
        assert_eq!(cancel_aura(1126), [0x66, 0x04, 0x00, 0x00]);

        // SMSG_UPDATE_AURA_DURATION: u8 slot, then u32 ms LE. Slot 3, 12_000 ms = 0x0000_2EE0.
        let body = [0x03, 0xE0, 0x2E, 0x00, 0x00];
        let mut r = &body[..];
        assert_eq!(read_update_aura_duration(&mut r).unwrap(), (3, 12_000));
        assert!(
            r.is_empty(),
            "the body is exactly 5 bytes — slot is a byte, not a dword"
        );
    }
}

/// One decoded `SMSG_SPELL_UPDATE_CHAIN_TARGETS` — the **beam's hop list** (decision 0955).
///
/// VERIFIED on both sides. Server: vmangos `Spell::SendChannelStart` (`Spell.cpp:4970-4997`) —
/// `ObjectGuid caster` (raw `u64`) · `u32 spellId` · `u32 count` · that many raw `u64` target
/// guids. Client: the handler `0x6e9820` decodes exactly that shape (`GetGuid` · `GetInt32` ·
/// `GetInt32` · `count × GetGuid`) and hands it to the array filler `0x605780`, which writes the
/// growable array at `unit+0xd44`. The chain `CharProc` (`0x60da79`) then consumes it once and
/// zeroes the count (`0x60db72`).
///
/// **This packet is NOT the only producer** (wow-re `chain-beam-law.md`, which refuted that
/// reading at the bytes): `0x605780` has exactly two callers — this handler's `0x605767`, and
/// **`0x6e800d`, inside `HandleSpellGo` `0x6e7a70`**. The client fills the same hop array from
/// `SMSG_SPELL_GO`'s own hit list, which is what makes a chain spell draw on its very first cast.
/// That matters here because vmangos only ever *sends* this packet from `SendChannelStart`, gated
/// on `SPELL_ATTR_EX_IS_CHANNELED` — so on this server the channel beams (Drain Life, Mind Flay,
/// Health Funnel, Corruption…) arrive by packet and the cast-stage chains (Chain Lightning, Chain
/// Heal, Chain Burn) arrive by the GO path. Both are the reference's own mechanism; benilla's
/// GO-derived hops are **not** a divergence (decision 0955).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpellChainTargets {
    pub caster: u64,
    pub spell_id: u32,
    /// The hop list in wire order. The client's filler (`0x605780`) walks it dropping any entry
    /// equal to the caster's own guid, so that filter belongs to the consumer, not to this decode.
    pub targets: Vec<u64>,
}

/// Read `SMSG_SPELL_UPDATE_CHAIN_TARGETS`. Guids are **raw** `u64` on both ends of the list — the
/// server writes `ObjectGuid` and the client reads `GetGuid` (0x4190b0), never the packed form.
pub(super) fn read_spell_chain_targets(r: &mut impl Read) -> io::Result<SpellChainTargets> {
    let caster = read_u64_le(r)?;
    let spell_id = read_u32_le(r)?;
    let count = read_u32_le(r)?;
    // The count is server-written and unbounded on the wire; read defensively rather than
    // pre-allocating from it (the `with_capacity` idiom the rest of this module uses is safe only
    // where a `u8` bounds the count).
    let mut targets = Vec::new();
    for _ in 0..count {
        targets.push(read_u64_le(r)?);
    }
    Ok(SpellChainTargets {
        caster,
        spell_id,
        targets,
    })
}
